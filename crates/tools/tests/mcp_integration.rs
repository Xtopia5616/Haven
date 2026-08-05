use haven_common::{McpServerConfig, McpTransportType};
use haven_tools::mcp::McpClient;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio_util::sync::CancellationToken;

/// Resolve the actual python interpreter path.
///
/// On some Windows setups `python` is a launcher stub that spawns the real
/// interpreter as a child process. Killing the stub then orphans the server
/// (it keeps its port bound, leaks processes, and makes port probing connect
/// to a stale server). Spawning the resolved interpreter directly makes
/// `kill()` deterministic and prevents orphans.
fn python_exe() -> &'static str {
    static PY: OnceLock<String> = OnceLock::new();
    PY.get_or_init(|| {
        for cmd in ["python", "python3"] {
            if let Ok(out) = std::process::Command::new(cmd)
                .arg("-c")
                .arg("import sys; print(sys.executable)")
                .output()
            {
                let exe = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !exe.is_empty() {
                    return exe;
                }
            }
        }
        "python".to_string()
    })
}

fn fixture_path() -> String {
    let p = std::env::current_dir().unwrap_or_default();
    // When running from workspace root, the tests dir is crates/tools/tests/
    // When running from crate root, it's tests/
    let candidates = [
        p.join("crates/tools/tests/fixtures/echo_mcp_server.py"),
        p.join("tests/fixtures/echo_mcp_server.py"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.to_string_lossy().to_string();
        }
    }
    // Fallback
    candidates[0].to_string_lossy().to_string()
}

fn http_fixture_path() -> String {
    let p = std::env::current_dir().unwrap_or_default();
    let candidates = [
        p.join("crates/tools/tests/fixtures/http_mcp_server.py"),
        p.join("tests/fixtures/http_mcp_server.py"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.to_string_lossy().to_string();
        }
    }
    candidates[0].to_string_lossy().to_string()
}

async fn create_client() -> Arc<McpClient> {
    let client = Arc::new(McpClient::new(&McpServerConfig {
        name: "echo-test".into(),
        command: python_exe().into(),
        args: vec![fixture_path()],
        ..Default::default()
    }));
    client.connect().await.unwrap();
    client
}

/// Kill a spawned child and, on Windows, its whole process tree. Some `python`
/// installs are launcher stubs that spawn the real interpreter as a child;
/// killing only the stub would orphan the server and keep its port alive.
#[cfg(windows)]
async fn kill_child_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(not(windows))]
async fn kill_child_tree(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Spawn the HTTP fixture and wait for it to report `READY <port>`. The
/// server binds to port 0, so the OS assigns a unique free port atomically —
/// no bind-then-release race with other tests. The READY line is the single
/// source of truth that the port belongs to this process.
async fn spawn_http_server() -> (u16, tokio::process::Child) {
    let mut child = tokio::process::Command::new(python_exe())
        .arg(http_fixture_path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn http fixture server");
    let stdout = child
        .stdout
        .take()
        .expect("failed to capture http fixture stdout");
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let line = tokio::time::timeout_at(deadline, lines.next_line()).await;
        match line {
            Ok(Ok(Some(l))) => {
                if let Some(p) = l.strip_prefix("READY ") {
                    match p.trim().parse::<u16>() {
                        Ok(port) => return (port, child),
                        Err(_) => break,
                    }
                }
            }
            // EOF or timeout: the server failed to start (e.g. missing python
            // module). Clean up the process tree and surface a clear error.
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
        }
    }
    kill_child_tree(&mut child).await;
    panic!("http fixture server did not report READY (startup failed)");
}

async fn create_http_client(port: u16) -> Arc<McpClient> {
    let client = Arc::new(McpClient::new(&McpServerConfig {
        name: "echo-http".into(),
        transport: McpTransportType::Http,
        url: format!("http://127.0.0.1:{}/mcp", port),
        ..Default::default()
    }));
    client.connect().await.unwrap();
    client
}

#[tokio::test]
async fn test_initialize_handshake() {
    let client = create_client().await;
    let status = client.status().await;
    assert!(matches!(status, haven_tools::McpClientStatus::Connected));
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_list_tools() {
    let client = create_client().await;
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 4);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[1].name, "reverse");
    assert_eq!(tools[2].name, "image");
    assert_eq!(tools[3].name, "resource");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_call_tool_echo() {
    let client = create_client().await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool("echo", serde_json::json!({"text": "hello world"}), cancel)
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output["text"], "hello world");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_call_tool_reverse() {
    let client = create_client().await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool("reverse", serde_json::json!({"text": "hello"}), cancel)
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output["text"], "olleh");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_call_tool_not_found() {
    let client = create_client().await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool("nonexistent", serde_json::json!({}), cancel)
        .await;
    assert!(result.is_err());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_shutdown_cleanup() {
    let client = create_client().await;
    client.shutdown().await.unwrap();
    // After shutdown, the process should be dead
    assert!(!client.is_alive().await);
}

#[tokio::test]
async fn test_process_detection() {
    let client = create_client().await;
    assert!(client.is_alive().await);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_call_tool_image_content() {
    let client = create_client().await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool("image", serde_json::json!({}), cancel)
        .await
        .unwrap();
    assert!(result.success);
    let images = result.output["images"].as_array().unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["type"], "image");
    assert_eq!(images[0]["mimeType"], "image/png");
    let data = images[0]["data"].as_str().unwrap();
    assert!(!data.is_empty());
    // The text summary carries a marker so the text-only agent loop knows an
    // image block was returned.
    let text = result.output["text"].as_str().unwrap();
    assert!(text.contains("[image block returned: image/png"));
    assert!(result.output["content"].is_array());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_call_tool_resource_content() {
    let client = create_client().await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool(
            "resource",
            serde_json::json!({"text": "hello note"}),
            cancel,
        )
        .await
        .unwrap();
    assert!(result.success);
    // Text resources fold into the plain-text summary.
    assert_eq!(result.output["text"], "hello note");
    assert!(result.output.get("resources").is_none());
    client.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Streamable HTTP transport tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_initialize_handshake() {
    let (port, mut server) = spawn_http_server().await;
    let client = create_http_client(port).await;
    assert!(matches!(
        client.status().await,
        haven_tools::McpClientStatus::Connected
    ));
    let snap = client.snapshot().await;
    assert_eq!(snap.transport, "http");
    client.shutdown().await.unwrap();
    kill_child_tree(&mut server).await;
}

#[tokio::test]
async fn test_http_list_tools() {
    let (port, mut server) = spawn_http_server().await;
    let client = create_http_client(port).await;
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 4);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[1].name, "reverse");
    assert_eq!(tools[2].name, "image");
    assert_eq!(tools[3].name, "resource");
    client.shutdown().await.unwrap();
    kill_child_tree(&mut server).await;
}

#[tokio::test]
async fn test_http_call_tool_echo() {
    let (port, mut server) = spawn_http_server().await;
    let client = create_http_client(port).await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool("echo", serde_json::json!({"text": "hello http"}), cancel)
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output["text"], "hello http");
    client.shutdown().await.unwrap();
    kill_child_tree(&mut server).await;
}

#[tokio::test]
async fn test_http_call_tool_reverse() {
    let (port, mut server) = spawn_http_server().await;
    let client = create_http_client(port).await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool("reverse", serde_json::json!({"text": "hello"}), cancel)
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output["text"], "olleh");
    client.shutdown().await.unwrap();
    kill_child_tree(&mut server).await;
}

#[tokio::test]
async fn test_http_call_tool_not_found() {
    let (port, mut server) = spawn_http_server().await;
    let client = create_http_client(port).await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool("nonexistent", serde_json::json!({}), cancel)
        .await;
    assert!(result.is_err());
    client.shutdown().await.unwrap();
    kill_child_tree(&mut server).await;
}

#[tokio::test]
async fn test_http_call_tool_image_content() {
    let (port, mut server) = spawn_http_server().await;
    let client = create_http_client(port).await;
    let cancel = CancellationToken::new();
    let result = client
        .call_tool("image", serde_json::json!({}), cancel)
        .await
        .unwrap();
    assert!(result.success);
    let images = result.output["images"].as_array().unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["mimeType"], "image/png");
    let text = result.output["text"].as_str().unwrap();
    assert!(text.contains("[image block returned: image/png"));
    client.shutdown().await.unwrap();
    kill_child_tree(&mut server).await;
}

#[tokio::test]
async fn test_http_liveness() {
    let (port, mut server) = spawn_http_server().await;
    let client = create_http_client(port).await;
    assert!(client.is_alive().await);

    kill_child_tree(&mut server).await;
    // The endpoint is gone, so the liveness probe must eventually report it
    // dead. Poll to tolerate OS-level port-release timing.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !client.is_alive().await {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "liveness probe still reported alive after server was killed"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
