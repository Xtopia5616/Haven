use crate::client::{McpClient, RateLimiter};
use crate::manager::McpManager;
use crate::protocol::{
    McpClientStatus, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo, extract_mcp_content,
    jsonrpc_notification, jsonrpc_request,
};
use base64::Engine;
use haven_common::McpServerConfig;
use serde_json::json;
use std::time::{Duration, Instant};

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
    // UI / Tauri snapshots keep snake_case on the wire out of Haven.
    assert!(json.get("input_schema").is_some());
    assert!(json.get("inputSchema").is_none());
    let deserialized: McpToolInfo = serde_json::from_value(json).unwrap();
    assert_eq!(deserialized.name, "echo");
    assert_eq!(deserialized.description, "Echo input back");
    assert_eq!(deserialized.input_schema["type"], "object");
}

#[test]
fn mcp_tool_info_deserializes_wire_input_schema() {
    let wire = json!({
        "name": "echo",
        "description": "Echo input back",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        }
    });
    let info: McpToolInfo = serde_json::from_value(wire).unwrap();
    assert_eq!(info.name, "echo");
    assert_eq!(info.input_schema["type"], "object");
    assert_eq!(info.input_schema["properties"]["text"]["type"], "string");
}

#[test]
fn mcp_tool_info_missing_description_defaults_empty() {
    let wire = json!({
        "name": "bare",
        "inputSchema": {"type": "object"}
    });
    let info: McpToolInfo = serde_json::from_value(wire).unwrap();
    assert_eq!(info.description, "");
    assert_eq!(info.input_schema["type"], "object");
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
