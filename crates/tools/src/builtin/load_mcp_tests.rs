use super::*;
use crate::Tool;
use crate::ToolHandle;
use haven_common::config::McpServerConfig;
use haven_common::mcp::McpServerTransport;

fn tool_for_tests() -> LoadMcpTool {
    LoadMcpTool {
        mcp_manager: Arc::new(McpManager::new()),
        mcp_server_config_source: McpServerConfigSource::default(),
        registry: ToolRegistry::new(),
        session_tool_overlay: SessionToolOverlay::new(),
        max_tools_per_request: 128,
    }
}

fn fake_info(name: &str, desc: &str) -> haven_mcp::McpToolInfo {
    haven_mcp::McpToolInfo {
        name: name.into(),
        description: desc.into(),
        input_schema: serde_json::json!({"type": "object"}),
    }
}

fn tool_with_source(source: McpServerConfigSource) -> LoadMcpTool {
    LoadMcpTool {
        mcp_manager: Arc::new(McpManager::new()),
        mcp_server_config_source: source,
        registry: ToolRegistry::new(),
        session_tool_overlay: SessionToolOverlay::new(),
        max_tools_per_request: 128,
    }
}

#[test]
fn test_load_mcp_name() {
    let tool = tool_for_tests();
    assert_eq!(tool.name(), "load_mcp");
}

#[test]
fn test_load_mcp_input_schema() {
    let tool = tool_for_tests();
    let schema = tool.input_schema();
    assert!(schema["properties"]["server_name"].is_object());
    assert!(schema["properties"]["tool_names"].is_object());
    assert_eq!(schema["required"][0], "server_name");
    assert!(
        schema.get("_session_id").is_none(),
        "private _session_id must not leak into the LLM schema"
    );
}

#[test]
fn test_load_mcp_requires_session_id() {
    assert!(tool_for_tests().requires_session_id());
}

#[test]
fn test_load_mcp_registrations_empty_after_inline_activate() {
    let tool = tool_for_tests();
    let regs = tool.registrations(&serde_json::json!({"server_name": "srv"}));
    assert!(regs.is_empty());
}

#[test]
fn test_normalize_tool_names_omitted_is_all() {
    assert_eq!(normalize_tool_names(None).unwrap(), None);
}

#[test]
fn test_normalize_tool_names_explicit_empty_errors() {
    assert!(normalize_tool_names(Some(vec![])).is_err());
    assert!(normalize_tool_names(Some(vec!["  ".into()])).is_err());
}

#[test]
fn test_normalize_tool_names_dedupes() {
    let names =
        normalize_tool_names(Some(vec!["a".into(), " a ".into(), "b".into(), "a".into()])).unwrap();
    assert_eq!(names, Some(vec!["a".into(), "b".into()]));
}

#[test]
fn test_select_tools_subset_and_missing() {
    let all = vec![
        fake_info("alpha", "A"),
        fake_info("beta", "B"),
        fake_info("gamma", "C"),
    ];
    let selection = select_tools(&all, Some(&["beta".into(), "nope".into(), "alpha".into()]));
    assert_eq!(
        selection
            .selected_tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
    // Selection follows server order; missing keeps request order.
    assert_eq!(selection.missing_tool_names, vec!["nope".to_string()]);
}

#[test]
fn test_select_tools_all_when_unfiltered() {
    let all = vec![fake_info("a", ""), fake_info("b", "")];
    let selection = select_tools(&all, None);
    assert_eq!(selection.selected_tools.len(), 2);
    assert!(selection.missing_tool_names.is_empty());
}

#[tokio::test]
async fn test_load_mcp_rejects_disabled() {
    let mcp_server_config_source = McpServerConfigSource::default();
    mcp_server_config_source
        .upsert_fixture(McpServerConfig {
            name: "srv".into(),
            enabled: false,
            ..Default::default()
        })
        .await;
    let tool = tool_with_source(mcp_server_config_source);
    let result = tool
        .run(
            LoadMcpParams {
                server_name: "srv".into(),
                tool_names: None,
                session_id: Some("ses-f6a1fdb2589b63c2ebaead556897b633".into()),
            },
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err(), "disabled server should be rejected");
    assert!(result.unwrap_err().to_string().contains("disabled"));
}

#[tokio::test]
async fn test_load_mcp_rejects_unknown() {
    let tool = tool_for_tests();
    let result = tool
        .run(
            LoadMcpParams {
                server_name: "nope".into(),
                tool_names: None,
                session_id: Some("ses-f6a1fdb2589b63c2ebaead556897b633".into()),
            },
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err(), "unknown server should be rejected");
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[tokio::test]
async fn test_load_mcp_requires_session_context() {
    let tool = tool_for_tests();
    let result = tool
        .run(
            LoadMcpParams {
                server_name: "nope".into(),
                tool_names: None,
                session_id: None,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("session context"));
}

#[tokio::test]
async fn authorization_contract_uses_trusted_transport_and_network_policy_stays_independent() {
    let source = McpServerConfigSource::default();
    source
        .upsert_fixture(McpServerConfig {
            name: "remote".into(),
            transport: McpServerTransport::Http,
            url: "http://127.0.0.1:1/mcp".into(),
            ..Default::default()
        })
        .await;
    let tool = tool_with_source(source.clone());
    let input = serde_json::json!({"server_name": "remote"});
    let contract = tool.authorization_contract(&input).await.unwrap();
    assert_eq!(
        contract.policy.execution_boundary,
        crate::ExecutionBoundary::InProcess
    );
    assert_eq!(contract.policy.network_access, crate::NetworkAccess::Public);

    let engine = crate::AuthorizationEngine::new();
    engine
        .set_boundaries(
            haven_common::types::SandboxMode::WorkspaceWrite,
            Vec::new(),
            haven_common::types::NetworkPolicy::Restricted,
        )
        .await;
    let request =
        crate::AuthorizationRequest::new(None, "load_mcp", input.clone(), contract.policy.clone())
            .with_target_binding(contract.target_binding.clone());
    assert!(matches!(
        engine.authorize_for_execution(&request).await.decision,
        crate::AuthorizationDecision::RequiresConfirmation { .. }
    ));

    engine
        .set_boundaries(
            haven_common::types::SandboxMode::WorkspaceWrite,
            Vec::new(),
            haven_common::types::NetworkPolicy::Deny,
        )
        .await;
    assert!(matches!(
        engine.authorize_for_execution(&request).await.decision,
        crate::AuthorizationDecision::Blocked {
            reason_code: crate::AuthorizationReasonCode::NetworkPolicy,
            ..
        }
    ));

    source
        .upsert_fixture(McpServerConfig {
            name: "remote".into(),
            transport: McpServerTransport::Stdio,
            command: "never-spawned".into(),
            ..Default::default()
        })
        .await;
    let stdio_contract = tool.authorization_contract(&input).await.unwrap();
    assert_eq!(
        stdio_contract.policy.execution_boundary,
        crate::ExecutionBoundary::HostProcess
    );
    engine
        .set_boundaries(
            haven_common::types::SandboxMode::WorkspaceWrite,
            Vec::new(),
            haven_common::types::NetworkPolicy::Open,
        )
        .await;
    let stdio_request =
        crate::AuthorizationRequest::new(None, "load_mcp", input, stdio_contract.policy)
            .with_target_binding(stdio_contract.target_binding);
    assert!(matches!(
        engine
            .authorize_for_execution(&stdio_request)
            .await
            .decision,
        crate::AuthorizationDecision::Blocked {
            reason_code: crate::AuthorizationReasonCode::SandboxBoundary,
            ..
        }
    ));
}

#[tokio::test]
async fn stale_transport_binding_fails_before_connecting_or_spawning() {
    let source = McpServerConfigSource::default();
    source
        .upsert_fixture(McpServerConfig {
            name: "remote".into(),
            transport: McpServerTransport::Http,
            url: "http://127.0.0.1:1/mcp".into(),
            ..Default::default()
        })
        .await;
    let tool = tool_with_source(source.clone());
    let input = serde_json::json!({
        "server_name": "remote",
        "_session_id": "ses-f6a1fdb2589b63c2ebaead556897b633"
    });
    let contract = tool.authorization_contract(&input).await.unwrap();

    source
        .upsert_fixture(McpServerConfig {
            name: "remote".into(),
            transport: McpServerTransport::Stdio,
            command: "must-not-spawn".into(),
            ..Default::default()
        })
        .await;
    let error = tool
        .execute_with_authorization_target(
            input,
            CancellationToken::new(),
            contract.target_binding.as_deref(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed after authorization"));
    assert!(!tool.mcp_manager.has_client("remote").await);
}

#[tokio::test]
async fn test_activate_refuses_oversized_add() {
    let session_tool_overlay = SessionToolOverlay::new();
    let padding_tools = (0..5)
        .map(|index| {
            Arc::new(crate::tool_contract::tests::MockTool::new(&format!(
                "pad_{index}"
            ))) as ToolHandle
        })
        .collect();
    session_tool_overlay
        .register_many_if_within_budget("ses-f6a1fdb2589b63c2ebaead556897b633", 0, 6, padding_tools)
        .await
        .unwrap();

    let proposed_names = vec!["new_a".into(), "new_b".into(), "new_c".into()];
    let budget = session_tool_overlay
        .preview_registration_budget(
            "ses-f6a1fdb2589b63c2ebaead556897b633",
            0,
            6,
            &proposed_names,
        )
        .await;
    assert!(budget.exceeds_limit());
    assert_eq!(budget.session_count, 5);
    assert_eq!(budget.net_new, 3);

    let attempt = session_tool_overlay
        .register_many_if_within_budget(
            "ses-f6a1fdb2589b63c2ebaead556897b633",
            0,
            6,
            proposed_names
                .iter()
                .map(|name| {
                    Arc::new(crate::tool_contract::tests::MockTool::new(name)) as ToolHandle
                })
                .collect(),
        )
        .await;
    assert_eq!(attempt.unwrap_err(), budget);
    assert_eq!(
        session_tool_overlay
            .list("ses-f6a1fdb2589b63c2ebaead556897b633")
            .await
            .len(),
        5
    );
}

#[tokio::test]
async fn session_overlay_registration_owns_version_updates() {
    let session_tool_overlay = SessionToolOverlay::new();
    let before = session_tool_overlay
        .catalog_version_for_session("ses-f6a1fdb2589b63c2ebaead556897b633")
        .await
        .session_overlay_version;
    let registered = session_tool_overlay
        .register_many_if_within_budget(
            "ses-f6a1fdb2589b63c2ebaead556897b633",
            0,
            10,
            vec![Arc::new(crate::builtin::notify::NotifyTool) as ToolHandle],
        )
        .await
        .unwrap();
    let after = session_tool_overlay
        .catalog_version_for_session("ses-f6a1fdb2589b63c2ebaead556897b633")
        .await
        .session_overlay_version;

    assert_eq!(registered, vec!["notify"]);
    assert_eq!(after, before + 1);
}

#[test]
fn test_catalog_entries_truncates_long_descriptions() {
    let long = "x".repeat(200);
    let entries = catalog_entries(&[fake_info("t", &long)]);
    let desc = entries[0]["description"].as_str().unwrap();
    assert!(desc.ends_with("..."));
    assert!(desc.chars().count() <= 160);
}

#[test]
fn legacy_loader_only_lists_deferred_tools() {
    let exposure = McpExposureConfig {
        default: McpToolExposure::Hidden,
        tools: std::collections::BTreeMap::from([
            ("read_*".into(), McpToolExposure::Direct),
            ("read_private".into(), McpToolExposure::Hidden),
            ("batch_*".into(), McpToolExposure::Deferred),
            ("code_*".into(), McpToolExposure::CodeMode),
        ]),
    };
    let available = vec![
        fake_info("read_public", "Direct"),
        fake_info("read_private", "Hidden exact override"),
        fake_info("batch_update", "Deferred"),
        fake_info("code_run", "CodeMode"),
        fake_info("unknown", "Hidden by default"),
    ];

    let visible = deferred_tools_for_exposure(available, &exposure);

    assert_eq!(
        visible
            .iter()
            .map(|info| info.name.as_str())
            .collect::<Vec<_>>(),
        ["batch_update"]
    );
}
