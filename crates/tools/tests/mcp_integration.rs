use haven_common::{McpServerConfig, McpTransportType};
use haven_mcp::McpClient;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// A local Streamable HTTP MCP server used by the client integration tests.
/// Keeping it in-process makes these tests independent of Python, PATH, and
/// external child-process permissions while exercising the production HTTP
/// transport and JSON-RPC content mapping end to end.
struct TestMcpServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl TestMcpServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_connection(stream));
            }
        });
        Self { url, task }
    }

    fn stop(self) {
        self.task.abort();
    }
}

impl Drop for TestMcpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_connection(mut stream: TcpStream) {
    const HEADER_END: &[u8] = b"\r\n\r\n";
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(end) = bytes
            .windows(HEADER_END.len())
            .position(|window| window == HEADER_END)
        {
            break end + HEADER_END.len();
        }
        let mut chunk = [0_u8; 1024];
        let Ok(read) = stream.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..read]);
    };

    let header = String::from_utf8_lossy(&bytes[..header_end]);
    if !header.starts_with("POST ") {
        let _ = stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    }
    let content_length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 1024];
        let Ok(read) = stream.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }

    let request =
        serde_json::from_slice::<serde_json::Value>(&bytes[header_end..]).unwrap_or_default();
    if request.get("id").is_none() {
        let _ = stream
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    }

    let id = request["id"].clone();
    let response = match request["method"].as_str() {
        Some("initialize") => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "test-mcp", "version": "1.0.0"},
            },
        }),
        Some("tools/list") => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": [
                {"name": "echo", "description": "echo", "inputSchema": {"type": "object"}},
                {"name": "reverse", "description": "reverse", "inputSchema": {"type": "object"}},
                {"name": "image", "description": "image", "inputSchema": {"type": "object"}},
                {"name": "resource", "description": "resource", "inputSchema": {"type": "object"}},
            ]},
        }),
        Some("tools/call") => tool_call_response(id, &request),
        _ => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}}),
    };
    let body = serde_json::to_vec(&response).unwrap();
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nMcp-Session-Id: test-session\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(headers.as_bytes()).await;
    let _ = stream.write_all(&body).await;
}

fn tool_call_response(id: serde_json::Value, request: &serde_json::Value) -> serde_json::Value {
    let arguments = &request["params"]["arguments"];
    let content = match request["params"]["name"].as_str() {
        Some("echo") => serde_json::json!([{ "type": "text", "text": arguments["text"] }]),
        Some("reverse") => serde_json::json!([{
            "type": "text",
            "text": arguments["text"].as_str().unwrap_or_default().chars().rev().collect::<String>(),
        }]),
        Some("image") => serde_json::json!([{
            "type": "image",
            "mimeType": "image/png",
            "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
        }]),
        Some("resource") => serde_json::json!([{
            "type": "resource",
            "resource": {"uri": "memory://note", "mimeType": "text/plain", "text": arguments["text"]},
        }]),
        other => {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("Tool not found: {}", other.unwrap_or_default())},
            });
        }
    };
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"content": content}})
}

async fn create_client() -> (Arc<McpClient>, TestMcpServer) {
    let server = TestMcpServer::start().await;
    let client = Arc::new(McpClient::new(
        &McpServerConfig {
            name: "echo-http".into(),
            transport: McpTransportType::Http,
            url: server.url.clone(),
            ..Default::default()
        },
        2 * 1024 * 1024,
        2 * 1024 * 1024,
    ));
    client.connect().await.unwrap();
    (client, server)
}

#[tokio::test]
async fn initialize_handshake_and_tools_are_discovered() {
    let (client, _server) = create_client().await;
    assert!(matches!(
        client.status().await,
        haven_tools::McpClientStatus::Connected
    ));
    assert_eq!(client.snapshot().await.transport, "http");
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 4);
    assert_eq!(tools[0].name, "echo");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn call_tool_echo_and_reverse() {
    let (client, _server) = create_client().await;
    let echo = client
        .call_tool(
            "echo",
            serde_json::json!({"text": "hello"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(echo.output["text"], "hello");
    let reverse = client
        .call_tool(
            "reverse",
            serde_json::json!({"text": "hello"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(reverse.output["text"], "olleh");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn call_tool_maps_image_and_resource_content() {
    let (client, _server) = create_client().await;
    let image = client
        .call_tool("image", serde_json::json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(image.output["images"][0]["mimeType"], "image/png");
    assert!(
        image.output["text"]
            .as_str()
            .unwrap()
            .contains("[image block returned: image/png")
    );
    let resource = client
        .call_tool(
            "resource",
            serde_json::json!({"text": "hello note"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(resource.output["text"], "hello note");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn call_tool_reports_unknown_tool() {
    let (client, _server) = create_client().await;
    assert!(
        client
            .call_tool("missing", serde_json::json!({}), CancellationToken::new())
            .await
            .is_err()
    );
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_marks_http_client_not_alive() {
    let (client, _server) = create_client().await;
    client.shutdown().await.unwrap();
    assert!(!client.is_alive().await);
}

#[tokio::test]
async fn liveness_detects_server_shutdown() {
    let (client, server) = create_client().await;
    assert!(client.is_alive().await);
    server.stop();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while client.is_alive().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "liveness probe still reported alive after server shutdown"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    client.shutdown().await.unwrap();
}
