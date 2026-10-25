use crate::{
    CODEMODE_RUN_TOOL_NAME, McpManager, McpToolAdapter, McpToolInfo, ToolHandle, ToolsFacade,
};
use haven_common::config::{McpExposureConfig, McpServerConfig};
use haven_common::mcp::{
    McpCatalogPresentation as Catalog, McpInvocationAdapters as Adapters, McpServerTransport,
    McpToolAvailability as Availability, McpToolExposure,
};
use haven_common::types::{NetworkPolicy, SandboxMode};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

fn exposure(catalog: Catalog, invocation: Adapters) -> McpToolExposure {
    McpToolExposure {
        catalog,
        invocation,
        ..McpToolExposure::default()
    }
}

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
    for policy in [
        exposure(Catalog::Direct, Adapters::Native),
        exposure(Catalog::Deferred, Adapters::Native),
        exposure(Catalog::Hidden, Adapters::NativeAndCodeMode),
    ] {
        config.exposure.default = policy;
        facade.upsert_mcp_server_config(config.clone()).await;
        assert!(
            facade
                .get_tool_for_session(Some(session_id), &name)
                .await
                .is_some()
        );
    }
    for policy in [
        McpToolExposure {
            availability: Availability::Disabled,
            catalog: Catalog::Hidden,
            invocation: Adapters::None,
        },
        exposure(Catalog::Hidden, Adapters::CodeMode),
    ] {
        config.exposure.default = policy;
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
async fn load_mcp_authorization_resolves_transport_from_config_before_connecting() {
    let facade = ToolsFacade::new();
    let authorization = facade.share_services().authorization;
    authorization
        .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Open)
        .await;

    let http_config = McpServerConfig {
        name: "deferred_remote".into(),
        transport: McpServerTransport::Http,
        url: "http://127.0.0.1:1/mcp".into(),
        ..Default::default()
    };
    facade.upsert_mcp_server_config(http_config.clone()).await;
    facade.rebuild_catalog().await.unwrap();
    let input = serde_json::json!({"server_name": "deferred_remote"});
    assert!(
        facade
            .get_tool_for_session(None, "load_mcp")
            .await
            .is_some()
    );
    let http_request = facade
        .resolve_authorization_request(None, "load_mcp", &input)
        .await
        .expect("trusted HTTP profile should resolve before network activity");
    assert_eq!(
        http_request.policy.execution_boundary,
        crate::ExecutionBoundary::InProcess
    );
    assert_eq!(
        http_request.policy.network_access,
        crate::NetworkAccess::Public
    );
    assert!(http_request.target_binding.is_some());
    assert!(matches!(
        authorization
            .authorize_for_execution(&http_request)
            .await
            .decision,
        crate::AuthorizationDecision::RequiresConfirmation { .. }
    ));

    facade
        .upsert_mcp_server_config(McpServerConfig {
            transport: McpServerTransport::Stdio,
            command: "must-not-spawn".into(),
            ..http_config
        })
        .await;
    authorization
        .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Deny)
        .await;
    let stdio_request = facade
        .resolve_authorization_request(None, "load_mcp", &input)
        .await
        .expect("trusted stdio profile should resolve to its process boundary");
    assert_eq!(
        stdio_request.policy.execution_boundary,
        crate::ExecutionBoundary::SandboxedHost
    );
    assert_eq!(
        stdio_request.policy.network_access,
        crate::NetworkAccess::None
    );
    assert_ne!(http_request.target_binding, stdio_request.target_binding);
    assert!(!matches!(
        authorization
            .authorize_for_execution(&stdio_request)
            .await
            .decision,
        crate::AuthorizationDecision::Blocked {
            reason_code: crate::AuthorizationReasonCode::SandboxBoundary
                | crate::AuthorizationReasonCode::NetworkPolicy,
            ..
        }
    ));
    assert!(
        !facade
            .coordinator
            .builtins
            .mcp_manager
            .has_client("deferred_remote")
            .await,
        "policy resolution must not connect or spawn the configured server"
    );
}

#[tokio::test]
async fn load_mcp_rejects_a_live_client_from_a_different_config_generation() {
    let facade = ToolsFacade::new();
    let manager = facade.coordinator.builtins.mcp_manager.clone();
    manager.set_network_policy(NetworkPolicy::Open).await;
    let catalog = Arc::new(RwLock::new(vec![serde_json::json!({
        "name": "deferred_tool",
        "description": "Deferred tool",
        "inputSchema": {"type": "object"}
    })]));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let cancellation = CancellationToken::new();
    let server = tokio::spawn(serve_mcp_catalog(listener, catalog, cancellation.clone()));

    let original = McpServerConfig {
        name: "versioned_remote".into(),
        transport: McpServerTransport::Http,
        url: format!("http://{address}/mcp"),
        ..Default::default()
    };
    facade.upsert_mcp_server_config(original.clone()).await;
    facade.rebuild_catalog().await.unwrap();
    manager.connect_server(&original).await.unwrap();

    let changed = McpServerConfig {
        url: "http://127.0.0.1:1/changed".into(),
        ..original
    };
    facade.upsert_mcp_server_config(changed).await;
    let tool = facade
        .get_tool_for_session(None, "load_mcp")
        .await
        .expect("built-in deferred loader is installed");
    let input = serde_json::json!({
        "server_name": "versioned_remote",
        "_session_id": "ses-65dd7c6b35b99687a424c2fd8bdb08a9"
    });
    let contract = tool
        .authorization_contract(&input)
        .await
        .expect("updated HTTP config resolves independently from the live client");
    let error = tool
        .execute_with_authorization_target(
            input,
            CancellationToken::new(),
            contract.target_binding.as_deref(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different configuration"));
    assert!(manager.has_client("versioned_remote").await);

    manager.remove_client("versioned_remote").await;
    cancellation.cancel();
    let _ = server.await;
}

#[tokio::test]
async fn native_mcp_tools_are_independent_from_catalog_visibility_and_availability() {
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
            default: exposure(Catalog::Direct, Adapters::Native),
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

    let authorization = facade.share_services().authorization;
    authorization
        .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
        .await;
    let mut dual_adapter = config.clone();
    dual_adapter.exposure.default = exposure(Catalog::Direct, Adapters::NativeAndCodeMode);
    facade.upsert_mcp_server_config(dual_adapter).await;
    let snapshot = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        snapshot
            .provider_definitions()
            .iter()
            .any(|def| def.name == name),
        "a tool that supports both adapters remains available as a native call"
    );
    assert!(
        facade
            .get_tool_for_session(Some(session_id), &name)
            .await
            .is_some()
    );
    assert!(
        facade
            .get_codemode_subcall_tool_for_session(Some(session_id), &name)
            .await
            .is_some(),
        "the CodeMode adapter is independent of the native adapter"
    );
    let request = facade
        .resolve_authorization_request(Some(session_id), &name, &serde_json::json!({}))
        .await
        .expect("the ordinary model-facing authorization path remains available");
    let [
        crate::security::DeclaredResource::McpRemoteInvocation {
            target_fingerprint,
            request_fingerprint,
        },
    ] = request.resources.as_slice()
    else {
        panic!("MCP invocation must declare its exact remote target and parameters");
    };
    assert_eq!(target_fingerprint.len(), 64);
    assert_eq!(request_fingerprint.len(), 64);
    assert!(
        target_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert!(
        request_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    let different_arguments = facade
        .resolve_authorization_request(
            Some(session_id),
            &name,
            &serde_json::json!({"view": "month"}),
        )
        .await
        .expect("MCP arguments are included in the trusted remote-call resource");
    let [
        crate::security::DeclaredResource::McpRemoteInvocation {
            target_fingerprint: same_target,
            request_fingerprint: different_request,
        },
    ] = different_arguments.resources.as_slice()
    else {
        panic!("MCP invocation must retain one remote resource");
    };
    assert_eq!(same_target, target_fingerprint);
    assert_ne!(different_request, request_fingerprint);

    let mut hidden_catalog = config.clone();
    hidden_catalog.exposure.default = exposure(Catalog::Hidden, Adapters::Native);
    facade.upsert_mcp_server_config(hidden_catalog).await;
    let hidden_snapshot = facade.tool_catalog_snapshot(session_id).await;
    assert!(
        hidden_snapshot
            .provider_definitions()
            .iter()
            .any(|def| def.name == name),
        "a hidden directory entry remains callable through its enabled native adapter"
    );
    assert!(
        facade
            .get_tool_for_session(Some(session_id), &name)
            .await
            .is_some()
    );
    let directory = crate::builtin::discover_tools::DiscoverToolsTool {
        deferred_catalog: facade.coordinator.core.operations.deferred.clone(),
        registry: facade.coordinator.core.operations.installed.clone(),
        session_tool_overlay: facade
            .coordinator
            .core
            .operations
            .session_tool_overlay
            .clone(),
        max_tools_per_request: 32,
        mcp_manager: Arc::new(manager.clone()),
        mcp_server_config_source: facade.coordinator.builtins.mcp_server_config_source.clone(),
    };
    let directory_detail = directory
        .run(
            crate::builtin::discover_tools::DiscoverToolsParams {
                action: "describe".into(),
                level: None,
                source: Some("mcp".into()),
                query: None,
                name: Some(name.clone()),
                root: None,
                cursor: None,
                limit: None,
                revision: None,
                operations: None,
                roots: None,
                session_id: Some(session_id.into()),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        directory_detail.output["status"], "not_found",
        "hidden catalog entries stay out of general discovery"
    );

    let mut hidden = config;
    hidden.exposure.default = McpToolExposure {
        availability: Availability::Disabled,
        catalog: Catalog::Hidden,
        invocation: Adapters::None,
    };
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
            default: exposure(Catalog::Hidden, Adapters::CodeMode),
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
    hidden.exposure.default = McpToolExposure {
        availability: Availability::Disabled,
        catalog: Catalog::Hidden,
        invocation: Adapters::None,
    };
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

    manager.shutdown_all().await;
    cancellation.cancel();
    server.await.unwrap();
}
