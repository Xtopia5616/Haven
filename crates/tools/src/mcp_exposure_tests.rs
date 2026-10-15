use crate::{
    CODEMODE_RUN_TOOL_NAME, McpManager, McpToolAdapter, McpToolInfo, ToolHandle, ToolsFacade,
    WORKFLOW_RUN_TOOL_NAME,
};
use haven_common::config::{McpExposureConfig, McpServerConfig, ToolConfig};
use haven_common::mcp::{McpServerTransport, McpToolExposure};
use haven_common::types::NetworkPolicy;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

fn active_tool(manager: &McpManager, operation: &str) -> ToolHandle {
    Arc::new(McpToolAdapter::new(
        manager.clone(),
        "calendar",
        McpToolInfo {
            name: operation.into(),
            description: "Calendar operation".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
    ))
}

async fn serve_mcp_request(stream: TcpStream, catalog: Arc<RwLock<Vec<Value>>>) {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or_default() == 0 {
        return;
    }
    let mut content_length = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or_default() == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or_default();
        }
    }
    let mut body = vec![0; content_length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return;
    };
    let result = match request["method"].as_str() {
        Some("server/discover") => serde_json::json!({
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {"tools": {"listChanged": false}},
            "_meta": {
                "io.modelcontextprotocol/serverInfo": {
                    "name": "exposure-test-server",
                    "version": "1"
                }
            }
        }),
        Some("tools/list") => serde_json::json!({
            "tools": catalog.read().await.clone(),
            "ttlMs": 60_000,
            "cacheScope": "private"
        }),
        _ => serde_json::json!({}),
    };
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": result
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.len(),
        response
    );
    let _ = writer.write_all(response.as_bytes()).await;
}

async fn serve_mcp_catalog(
    listener: TcpListener,
    catalog: Arc<RwLock<Vec<Value>>>,
    cancellation: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { return; };
                tokio::spawn(serve_mcp_request(stream, catalog.clone()));
            }
        }
    }
}

#[tokio::test]
async fn stale_session_mcp_calls_follow_current_exposure_policy() {
    let facade = ToolsFacade::new();
    let manager = facade.coordinator.builtins.mcp_manager.clone();
    let session_id = "ses-65dd7c6b35b99687a424c2fd8bdb08a9";
    let tool = active_tool(&manager, "create_event");
    let name = tool.name();
    facade
        .coordinator
        .core
        .operations
        .session_tool_overlay
        .register(session_id, tool)
        .await;

    let mut config = McpServerConfig {
        name: "calendar".into(),
        ..Default::default()
    };
    for exposure in [
        McpToolExposure::Direct,
        McpToolExposure::ModelOnly,
        McpToolExposure::Deferred,
    ] {
        config.exposure.default = exposure;
        facade.upsert_mcp_server_config(config.clone()).await;
        assert!(
            facade
                .get_tool_for_session(Some(session_id), &name)
                .await
                .is_some()
        );
    }
    for exposure in [McpToolExposure::Hidden, McpToolExposure::CodeMode] {
        config.exposure.default = exposure;
        facade.upsert_mcp_server_config(config.clone()).await;
        assert!(
            facade
                .get_tool_for_session(Some(session_id), &name)
                .await
                .is_none()
        );
    }
}

#[tokio::test]
async fn direct_mcp_tools_are_advertised_and_hidden_tools_are_not_executable() {
    let facade = ToolsFacade::new();
    let manager = facade.coordinator.builtins.mcp_manager.clone();
    manager.set_network_policy(NetworkPolicy::Open).await;

    let catalog = Arc::new(RwLock::new(vec![serde_json::json!({
        "name": "calendar_status",
        "description": "Read calendar status",
        "inputSchema": {"type": "object"}
    })]));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let cancellation = CancellationToken::new();
    let server = tokio::spawn(serve_mcp_catalog(listener, catalog, cancellation.clone()));

    let config = McpServerConfig {
        name: "calendar".into(),
        transport: McpServerTransport::Http,
        url: format!("http://{address}/mcp"),
        exposure: McpExposureConfig {
            default: McpToolExposure::Direct,
            ..Default::default()
        },
        ..Default::default()
    };
    facade.upsert_mcp_server_config(config.clone()).await;
    manager.connect_server(&config).await.unwrap();

    let session_id = "ses-65dd7c6b35b99687a424c2fd8bdb08a9";
    let name = McpToolAdapter::qualified_name_of("calendar", "calendar_status");
    let snapshot = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        snapshot
            .provider_definitions()
            .iter()
            .any(|def| def.name == name)
    );
    assert!(
        facade
            .get_tool_for_session(Some(session_id), &name)
            .await
            .is_some()
    );

    let mut model_only = config.clone();
    model_only.exposure.default = McpToolExposure::ModelOnly;
    facade.upsert_mcp_server_config(model_only).await;
    let snapshot = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        snapshot
            .provider_definitions()
            .iter()
            .any(|def| def.name == name),
        "ModelOnly tools remain available as top-level model calls"
    );
    assert!(
        facade
            .get_tool_for_session(Some(session_id), &name)
            .await
            .is_some()
    );
    assert!(
        snapshot.get(WORKFLOW_RUN_TOOL_NAME).is_none(),
        "ModelOnly tools do not enter the nested Workflow allowlist"
    );
    assert!(
        facade
            .get_workflow_subcall_tool_for_session(Some(session_id), &name)
            .await
            .is_none(),
        "a stale Workflow plan is checked against live exposure"
    );
    assert!(
        facade
            .resolve_workflow_subcall_authorization_request(
                Some(session_id),
                &name,
                &serde_json::json!({})
            )
            .await
            .is_err(),
        "ModelOnly is rejected before a nested Workflow authorization is issued"
    );
    assert!(
        facade
            .resolve_authorization_request(Some(session_id), &name, &serde_json::json!({}))
            .await
            .is_ok(),
        "the ordinary model-facing authorization path remains available"
    );

    let mut hidden = config;
    hidden.exposure.default = McpToolExposure::Hidden;
    facade.upsert_mcp_server_config(hidden).await;
    let snapshot = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        snapshot
            .provider_definitions()
            .iter()
            .all(|def| def.name != name)
    );
    assert!(
        facade
            .get_tool_for_session(Some(session_id), &name)
            .await
            .is_none()
    );

    manager.shutdown_all().await;
    cancellation.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn codemode_exposure_is_only_visible_through_the_synthetic_bounded_tool() {
    let facade = ToolsFacade::new();
    let manager = facade.coordinator.builtins.mcp_manager.clone();
    manager.set_network_policy(NetworkPolicy::Open).await;

    let catalog = Arc::new(RwLock::new(vec![serde_json::json!({
        "name": "calendar_status",
        "description": "Read calendar status",
        "inputSchema": {"type": "object", "properties": {"view": {"type": "string"}}}
    })]));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let cancellation = CancellationToken::new();
    let server = tokio::spawn(serve_mcp_catalog(listener, catalog, cancellation.clone()));

    let config = McpServerConfig {
        name: "calendar".into(),
        transport: McpServerTransport::Http,
        url: format!("http://{address}/mcp"),
        exposure: McpExposureConfig {
            default: McpToolExposure::CodeMode,
            ..Default::default()
        },
        ..Default::default()
    };
    facade.upsert_mcp_server_config(config.clone()).await;
    manager.connect_server(&config).await.unwrap();

    let session_id = "ses-65dd7c6b35b99687a424c2fd8bdb08a9";
    let child_name = McpToolAdapter::qualified_name_of("calendar", "calendar_status");
    let snapshot = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        snapshot
            .provider_definitions()
            .iter()
            .any(|definition| definition.name == CODEMODE_RUN_TOOL_NAME)
    );
    assert!(
        snapshot
            .provider_definitions()
            .iter()
            .all(|definition| definition.name != child_name)
    );
    let code_mode_tool = snapshot
        .get(CODEMODE_RUN_TOOL_NAME)
        .expect("the synthetic tool is available when one MCP child is exposed");
    let definitions = code_mode_tool
        .codemode_tool_definitions()
        .expect("synthetic CodeMode tool has a trusted capability set");
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].name, child_name);

    assert!(
        facade
            .get_tool_for_session(Some(session_id), &child_name)
            .await
            .is_none(),
        "CodeMode children never enter the provider-native lookup"
    );
    assert!(
        facade
            .get_codemode_subcall_tool_for_session(Some(session_id), &child_name)
            .await
            .is_some()
    );
    let request = facade
        .resolve_codemode_subcall_authorization_request(
            Some(session_id),
            &child_name,
            &serde_json::json!({ "view": "agenda" }),
        )
        .await
        .expect("the child uses the ordinary MCP operation policy and live target");
    assert!(request.target_binding.is_some());

    let mut hidden = config;
    hidden.exposure.default = McpToolExposure::Hidden;
    facade.upsert_mcp_server_config(hidden).await;
    assert!(
        facade
            .resolve_codemode_subcall_authorization_request(
                Some(session_id),
                &child_name,
                &serde_json::json!({}),
            )
            .await
            .is_err(),
        "stale CodeMode allowlists must fail after exposure changes"
    );
    let snapshot = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        snapshot
            .provider_definitions()
            .iter()
            .all(|definition| definition.name != CODEMODE_RUN_TOOL_NAME)
    );
    assert!(
        snapshot
            .provider_definitions()
            .iter()
            .all(|definition| definition.name != WORKFLOW_RUN_TOOL_NAME),
        "CodeMode-only capabilities must not enter the Legacy Workflow surface"
    );

    manager.shutdown_all().await;
    cancellation.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn workflow_surface_tracks_direct_and_loaded_deferred_mcp_exposure() {
    let facade = ToolsFacade::new();
    let manager = facade.coordinator.builtins.mcp_manager.clone();
    manager.set_network_policy(NetworkPolicy::Open).await;

    let catalog = Arc::new(RwLock::new(vec![serde_json::json!({
        "name": "calendar_status",
        "description": "Read calendar status",
        "inputSchema": {"type": "object", "properties": {"view": {"type": "string"}}}
    })]));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let cancellation = CancellationToken::new();
    let server = tokio::spawn(serve_mcp_catalog(listener, catalog, cancellation.clone()));

    let config = McpServerConfig {
        name: "calendar".into(),
        transport: McpServerTransport::Http,
        url: format!("http://{address}/mcp"),
        exposure: McpExposureConfig {
            default: McpToolExposure::Direct,
            ..Default::default()
        },
        ..Default::default()
    };
    facade.upsert_mcp_server_config(config.clone()).await;
    manager.connect_server(&config).await.unwrap();

    let session_id = "ses-41414141414141414141414141414141";
    let child_name = McpToolAdapter::qualified_name_of("calendar", "calendar_status");
    let direct = facade.tool_catalog_snapshot(session_id).await;
    let workflow = direct
        .get(WORKFLOW_RUN_TOOL_NAME)
        .expect("Direct MCP capability exposes Workflow");
    assert_eq!(
        workflow
            .workflow_capability_definitions()
            .unwrap()
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>(),
        [child_name.as_str()]
    );

    let mut deferred_config = config.clone();
    deferred_config.exposure.default = McpToolExposure::Deferred;
    facade.upsert_mcp_server_config(deferred_config).await;
    let unloaded = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        unloaded.get(WORKFLOW_RUN_TOOL_NAME).is_none(),
        "unloaded Deferred tools do not enter a session Workflow allowlist"
    );
    assert!(
        facade
            .register_mcp_for_session(session_id, "calendar", None)
            .await
    );
    let loaded = facade.tool_catalog_snapshot(session_id).await;
    let workflow = loaded
        .get(WORKFLOW_RUN_TOOL_NAME)
        .expect("loaded Deferred MCP capability exposes Workflow");
    assert_eq!(
        workflow.workflow_capability_definitions().unwrap()[0].name,
        child_name
    );

    facade
        .coordinator
        .runtime
        .update_platform(|current| {
            let mut next = current.clone();
            next.tool_settings.insert(
                child_name.clone(),
                ToolConfig {
                    enabled: false,
                    ..Default::default()
                },
            );
            next
        })
        .await;
    let disabled = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        disabled.get(WORKFLOW_RUN_TOOL_NAME).is_none(),
        "disabled MCP tools do not enter the Workflow allowlist"
    );

    let mut hidden_config = config;
    hidden_config.exposure.default = McpToolExposure::Hidden;
    facade.upsert_mcp_server_config(hidden_config).await;
    let hidden = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        hidden.get(WORKFLOW_RUN_TOOL_NAME).is_none(),
        "Hidden capabilities are removed from both top-level and Workflow surfaces"
    );

    manager.shutdown_all().await;
    cancellation.cancel();
    server.await.unwrap();
}
