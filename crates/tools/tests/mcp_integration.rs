use haven_tools::mcp::McpClient;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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

async fn create_client() -> Arc<McpClient> {
    let client = Arc::new(McpClient::new(
        "echo-test",
        "python",
        &[fixture_path()],
        &[],
    ));
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
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[1].name, "reverse");
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
