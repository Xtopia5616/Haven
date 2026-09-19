use crate::protocol::{REQUEST_TIMEOUT_SECS, jsonrpc_notification, jsonrpc_request};
use crate::sse::SseParser;
use futures_util::StreamExt;
use haven_common::process_containment::ProcessContainment;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const MAX_STDIO_LINE_BYTES: usize = 1024 * 1024;
const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_NOTIFY_BODY_BYTES: usize = 64 * 1024;
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
    pub(crate) child: Child,
    pub(crate) _containment: ProcessContainment,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: BufReader<ChildStdout>,
    pub(crate) notification_tx: tokio::sync::mpsc::Sender<Value>,
    pub(crate) tools_list_changed: Arc<ToolsListChangedSignal>,
    pub(crate) cancel: CancellationToken,
}

impl StdioInner {
    pub(crate) async fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<Value> {
        let req = jsonrpc_request(id, method, params);
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        write_stdio_line(&mut self.stdin, &line, method, &self.cancel).await?;

        let timeout = tokio::time::Duration::from_secs(REQUEST_TIMEOUT_SECS);
        let cancel = self.cancel.clone();
        let result = tokio::select! {
            _ = cancel.cancelled() => {
                anyhow::bail!("MCP stdio request '{}' cancelled", method)
            }
            result = tokio::time::timeout(timeout, async {
            loop {
                let buf = read_line_bounded(&mut self.stdout).await?;
                let buf = String::from_utf8_lossy(&buf).trim().to_string();
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
                    enqueue_notification(
                        &self.notification_tx,
                        &self.tools_list_changed,
                        parsed,
                    )?;
                }
            }
        }) => result
        };

        match result {
            Ok(r) => r,
            Err(_) => anyhow::bail!("MCP request '{}' timed out", method),
        }
    }

    pub(crate) async fn notify(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<()> {
        let notification = jsonrpc_notification(method, params);
        let mut line = serde_json::to_string(&notification)?;
        line.push('\n');
        write_stdio_line(&mut self.stdin, &line, method, &self.cancel).await
    }
}

async fn write_stdio_line(
    stdin: &mut ChildStdin,
    line: &str,
    method: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    write_stdio_line_with_timeout(
        stdin,
        line,
        method,
        cancel,
        Duration::from_secs(REQUEST_TIMEOUT_SECS),
    )
    .await
}

async fn write_stdio_line_with_timeout<W: tokio::io::AsyncWrite + Unpin>(
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

/// Shared state for the Streamable HTTP transport: the reqwest client, the
/// endpoint URL, request headers (from the config `env` list), and the session
/// id returned by the server (`Mcp-Session-Id`, per the MCP Streamable HTTP
/// spec).
pub(crate) struct HttpShared {
    pub(crate) http: reqwest::Client,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) session_id: Arc<tokio::sync::Mutex<String>>,
    pub(crate) cancel: CancellationToken,
}

/// Streamable HTTP transport (MCP spec, transport 2025-03-26). Requests are
/// POSTed to the endpoint; the server replies with JSON or an SSE stream.
/// Server-to-client notifications arrive over a long-lived SSE stream opened
/// with a GET request.
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
            let body = read_text_bounded(resp, MAX_JSON_BODY_BYTES, &self.shared.cancel).await?;
            anyhow::bail!(
                "MCP HTTP error (status {}): {}",
                status.as_u16(),
                haven_common::error::sanitize_error_text(&body)
            );
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

    pub(crate) async fn notify(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<()> {
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
            let body = read_text_bounded(resp, MAX_JSON_BODY_BYTES, &self.shared.cancel).await?;
            anyhow::bail!(
                "MCP HTTP notify error (status {}): {}",
                status.as_u16(),
                haven_common::error::sanitize_error_text(&body)
            );
        }
        // Drain the body to release the connection for reuse. A failed drain
        // is still a transport failure: otherwise a broken connection is
        // reported as a successful notification and can poison the pool.
        read_bytes_bounded(resp, MAX_NOTIFY_BODY_BYTES, &self.shared.cancel).await?;
        Ok(())
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
    ) -> anyhow::Result<Value> {
        match self {
            McpClientInner::Stdio(s) => s.request(id, method, params).await,
            McpClientInner::Http(h) => h.request(id, method, params).await,
        }
    }

    pub(crate) async fn notify(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<()> {
        match self {
            McpClientInner::Stdio(s) => s.notify(method, params).await,
            McpClientInner::Http(h) => h.notify(method, params).await,
        }
    }
}

impl Drop for McpClientInner {
    fn drop(&mut self) {
        if let McpClientInner::Stdio(s) = self
            && let Err(error) = s.child.start_kill()
        {
            tracing::debug!(
                "MCP stdio child cleanup failed: {}",
                haven_common::error::sanitize_error_text(&error.to_string())
            );
        }
    }
}

/// Read an SSE streamed response body until the JSON-RPC response matching
/// `id` arrives; id-less messages in the stream are routed as notifications.
fn unpack_jsonrpc(value: Value) -> anyhow::Result<Value> {
    if let Some(err) = value.get("error") {
        let code = err["code"].as_i64().unwrap_or(-1);
        let msg = err["message"].as_str().unwrap_or("unknown error");
        anyhow::bail!(
            "MCP error ({}): {}",
            code,
            haven_common::error::sanitize_error_text(msg)
        );
    }
    Ok(value["result"].clone())
}

/// Attach the current MCP session id (if any) and every user-configured
/// header to a request builder. Shared by all HTTP transport call sites so
/// the session/auth header handling cannot drift.
pub(crate) async fn apply_http_session_headers(
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

/// Open a long-lived SSE stream (GET) and route every server-to-client message
/// into the notification channel. Ends when the stream closes or the cancel
/// token fires; the caller retries with backoff.
async fn listen_sse(
    shared: &Arc<HttpShared>,
    tx: &tokio::sync::mpsc::Sender<Value>,
    cancel: &CancellationToken,
    max_sse_buffer: usize,
    tools_list_changed: &ToolsListChangedSignal,
) -> anyhow::Result<()> {
    let builder = shared
        .http
        .get(&shared.url)
        .header("Accept", "text/event-stream");
    let req = apply_http_session_headers(builder, shared).await;
    let resp = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        result = tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), req.send()) => {
            result.map_err(|_| anyhow::anyhow!("MCP SSE connection timed out"))??
        }
    };
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
                            if cancel.is_cancelled() {
                                return Ok(());
                            }
                            enqueue_notification(tx, tools_list_changed, ev)?;
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
pub(crate) fn spawn_sse_listener(
    name: String,
    cancel: CancellationToken,
    shared: Arc<HttpShared>,
    notification_tx: tokio::sync::mpsc::Sender<Value>,
    max_sse_buffer: usize,
    tools_list_changed: Arc<ToolsListChangedSignal>,
) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(2);
        loop {
            if cancel.is_cancelled() {
                break;
            }
            match listen_sse(
                &shared,
                &notification_tx,
                &cancel,
                max_sse_buffer,
                &tools_list_changed,
            )
            .await
            {
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
            let jitter_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| u64::from(duration.subsec_millis() % 500))
                .unwrap_or(0);
            let delay = backoff.saturating_add(Duration::from_millis(jitter_ms));
            backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(delay) => {}
            }
        }
    });
}

/// Read one JSON-RPC line without allowing a peer to force an unbounded
/// allocation. The reader consumes the input in chunks so an oversized line
/// is rejected before it can grow past the configured cap.
async fn read_line_bounded(reader: &mut BufReader<ChildStdout>) -> std::io::Result<Vec<u8>> {
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
    if resp
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        anyhow::bail!("MCP response body exceeds the {} byte limit", limit);
    }
    let mut body = Vec::with_capacity(resp.content_length().unwrap_or(0) as usize);
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
        if chunk.len() > limit.saturating_sub(body.len()) {
            anyhow::bail!("MCP response body exceeds the {} byte limit", limit);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
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
    let builder = shared
        .http
        .get(&shared.url)
        .header("Accept", "text/event-stream");
    let req = apply_http_session_headers(builder, shared).await;
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::process::Command;

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
            session_id: Arc::new(tokio::sync::Mutex::new(String::new())),
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
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
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
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
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
