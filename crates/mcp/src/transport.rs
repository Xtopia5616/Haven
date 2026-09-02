use crate::protocol::{REQUEST_TIMEOUT_SECS, jsonrpc_notification, jsonrpc_request};
use crate::sse::SseParser;
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio_util::sync::CancellationToken;

/// stdio transport: a spawned child process speaking JSON-RPC over its stdin
/// and stdout pipes.
pub(crate) struct StdioInner {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: BufReader<ChildStdout>,
    pub(crate) notification_tx: tokio::sync::mpsc::UnboundedSender<Value>,
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

    pub(crate) async fn notify(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<()> {
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
    pub(crate) notification_tx: tokio::sync::mpsc::UnboundedSender<Value>,
    /// Single buffered (incomplete) SSE line/event cap (from context limits).
    pub(crate) max_sse_buffer: usize,
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
pub(crate) fn spawn_sse_listener(
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
pub(crate) async fn http_is_alive(shared: &Arc<HttpShared>) -> bool {
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
