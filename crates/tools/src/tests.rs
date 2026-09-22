use super::*;
use serde_json::json;
use std::collections::HashMap;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[test]
fn llm_tool_name_preserves_short_names_and_hashes_truncated_names() {
    assert_eq!(llm_tool_name("mcp::calendar::list"), "mcp__calendar__list");

    let first = llm_tool_name(&format!("mcp::{}::read", "a".repeat(100)));
    let second = llm_tool_name(&format!("mcp::{}::read", "a".repeat(99) + "b"));
    assert_eq!(first.len(), 64);
    assert_eq!(second.len(), 64);
    assert!(first.starts_with("mcp__"));
    assert_ne!(first, second);
}

fn budget_test_def(name: &str, source: ToolSource) -> ToolDef {
    ToolDef::new(
        name,
        format!("test tool {name}"),
        json!({"type": "object"}),
        RiskLevel::Safe,
    )
    .with_manifest(ToolManifest {
        identity: ToolIdentity {
            source,
            catalog_group: haven_common::tools::ToolCatalogGroup::Other,
            root: name.split('.').next().unwrap_or(name).into(),
            operation: None,
            stable_name: name.into(),
        },
        model: ToolModel {
            name: name.into(),
            description: format!("test tool {name}"),
            input_schema: json!({"type": "object"}),
        },
        policy: ToolPolicy {
            risk_level: RiskLevel::Safe,
            permission_key: name.into(),
            confirmation: "none".into(),
            idempotency: "safe".into(),
            scope: "session".into(),
            concurrency: "exclusive".into(),
            effect: "read_only".into(),
            data_sensitivity: "none".into(),
            network_access: "none".into(),
        },
        presentation: ToolPresentation {
            label: name.into(),
            renderer: "tools".into(),
            icon: "tools".into(),
            represented_source: ToolSource::Builtin,
        },
        root_presentation: haven_common::tools::ToolRootPresentation {
            label: name.split('.').next().unwrap_or(name).into(),
            description: format!("{} test capabilities", name),
            icon: "tools".into(),
        },
        prompt: haven_common::tools::ToolPrompt {
            when_to_use: "test".into(),
            when_not_to_use: "never".into(),
            key_operations: vec![name.into()],
        },
        availability: ToolAvailability::default(),
    })
}

#[test]
fn tool_budget_selection_is_source_aware_and_deterministic() {
    let selection = select_tool_defs_for_budget(
        vec![
            budget_test_def("skill__z", ToolSource::Skill),
            budget_test_def("core.z", ToolSource::Builtin),
            budget_test_def("skill__a", ToolSource::Skill),
            budget_test_def("core.a", ToolSource::Builtin),
        ],
        vec![
            budget_test_def("mcp__z", ToolSource::Mcp),
            budget_test_def("mcp__a", ToolSource::Mcp),
        ],
        4,
    );

    let selected: Vec<_> = selection
        .selected
        .iter()
        .map(|def| def.name.as_str())
        .collect();
    assert_eq!(selected, ["core.a", "core.z", "mcp__a", "mcp__z"]);
    assert_eq!(selection.omitted, ["skill__a", "skill__z"]);
    assert_eq!(selection.omitted_core, 0);
}

#[test]
fn tool_budget_selection_reports_core_omissions_when_core_exceeds_limit() {
    let selection = select_tool_defs_for_budget(
        vec![
            budget_test_def("core.c", ToolSource::Builtin),
            budget_test_def("skill__a", ToolSource::Skill),
            budget_test_def("core.a", ToolSource::Builtin),
            budget_test_def("core.b", ToolSource::Builtin),
        ],
        Vec::new(),
        2,
    );

    let selected: Vec<_> = selection
        .selected
        .iter()
        .map(|def| def.name.as_str())
        .collect();
    assert_eq!(selected, ["core.a", "core.b"]);
    assert_eq!(selection.omitted, ["core.c", "skill__a"]);
    assert_eq!(selection.omitted_core, 1);
}

#[tokio::test]
async fn test_tools_manager_new() {
    let mgr = ToolsManager::new();
    let tools = mgr.registry().list().await;
    assert!(tools.is_empty());
}

#[tokio::test]
async fn scoped_catalog_rebuild_reuses_unaffected_runtime_instances() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;
    let ask_before = mgr.registry().get("ask").await.unwrap();
    let shell_before = mgr.get_tool("shell").await.unwrap();

    mgr.set_default_shell(ShellChoice::default()).await;

    let ask_after = mgr.registry().get("ask").await.unwrap();
    let shell_after = mgr.get_tool("shell").await.unwrap();
    assert!(Arc::ptr_eq(&ask_before, &ask_after));
    assert!(!Arc::ptr_eq(&shell_before, &shell_after));
}

#[tokio::test]
async fn manager_control_plane_errors_preserve_structured_metadata() {
    let mgr = ToolsManager::new();
    let missing = mgr
        .execute_tool(None, "missing", json!({}), CancellationToken::new())
        .await
        .expect_err("missing tool must fail");
    let missing_metadata = missing
        .downcast_ref::<StructuredToolError>()
        .expect("missing tool error must carry metadata")
        .metadata();
    assert_eq!(missing_metadata, ToolErrorMetadata::other());

    mgr.set_tool_settings(HashMap::from([(
        "ask".into(),
        ToolConfig {
            enabled: false,
            ..Default::default()
        },
    )]))
    .await;
    let disabled = mgr
        .execute_tool(None, "ask", json!({}), CancellationToken::new())
        .await
        .expect_err("disabled tool must fail");
    let disabled_metadata = disabled
        .downcast_ref::<StructuredToolError>()
        .expect("disabled tool error must carry metadata")
        .metadata();
    assert_eq!(disabled_metadata.class, ToolErrorClass::Permission);
    assert_eq!(disabled_metadata.outcome, ToolExecutionOutcome::Failed);
    assert_eq!(
        disabled_metadata.retryability,
        ToolRetryability::NotRetryable
    );
}

#[tokio::test]
async fn runtime_capabilities_report_unavailable_backends_explicitly() {
    let mgr = ToolsManager::new();
    let capabilities = mgr.runtime_capabilities().await;
    assert!(!capabilities.vision);
    assert!(!capabilities.image_generation);
    assert!(!capabilities.transcription);
    assert!(!capabilities.recording);
    assert!(!capabilities.tts);
    assert_eq!(
        capabilities.web_search,
        "unavailable (no provider builtin search; no MCP search server)"
    );
}

#[tokio::test]
async fn tool_catalog_snapshot_captures_lookup_policy_and_manifest() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    let snapshot = mgr.tool_catalog_snapshot("ses-snapshot").await;
    let tool = snapshot
        .get("ask")
        .expect("core tool must be present in the session snapshot");
    assert_eq!(tool.name(), "ask");
    assert!(!snapshot.is_empty());

    let input = json!({"question": "continue?"});
    let policy = snapshot.operation_policy("ask", &input);
    assert_eq!(policy.scope, ToolOperationScope::Session);
    assert_eq!(
        snapshot
            .manifest("ask")
            .expect("snapshot must retain renderer metadata")
            .identity
            .stable_name,
        "ask"
    );
}

#[tokio::test]
async fn tool_catalog_snapshot_keeps_provider_surface_stable_after_drift() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;
    let before = mgr.tool_catalog_snapshot("ses-drift").await;
    assert!(
        before
            .provider_definitions()
            .iter()
            .all(|definition| definition.name != "drift_only")
    );

    struct DriftTool;
    #[async_trait::async_trait]
    impl Tool for DriftTool {
        fn name(&self) -> String {
            "drift_only".into()
        }
        fn description(&self) -> String {
            "catalog drift probe".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({})))
        }
    }

    mgr.register_for_session("ses-drift", Arc::new(DriftTool))
        .await;
    let after = mgr.tool_catalog_snapshot("ses-drift").await;
    assert!(
        after
            .provider_definitions()
            .iter()
            .any(|definition| definition.name == "drift_only")
    );
    assert!(
        before
            .provider_definitions()
            .iter()
            .all(|definition| definition.name != "drift_only"),
        "the prepared Turn snapshot must not change when the registry mutates"
    );
    assert_ne!(before.version(), after.version());
}

#[tokio::test]
async fn catalog_drift_reaches_the_live_execution_boundary() {
    struct DriftTool(&'static str);

    #[async_trait::async_trait]
    impl Tool for DriftTool {
        fn name(&self) -> String {
            "drift_execution".into()
        }
        fn description(&self) -> String {
            format!("catalog version {}", self.0)
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({"implementation": self.0})))
        }
    }

    let mgr = ToolsManager::new();
    mgr.registry()
        .register(Arc::new(DriftTool("prepared")))
        .await
        .unwrap();
    let catalog = mgr.tool_catalog_snapshot("ses-drift-execution").await;
    assert_eq!(
        catalog.get("drift_execution").unwrap().description(),
        "catalog version prepared"
    );

    // The prepared provider surface remains immutable, but execution must
    // consult the current session overlay at the safety boundary.
    mgr.register_for_session("ses-drift-execution", Arc::new(DriftTool("live")))
        .await;
    let result = mgr
        .execute_tool(
            Some("ses-drift-execution"),
            "drift_execution",
            json!({}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.output["implementation"], "live");
    assert_eq!(
        catalog.get("drift_execution").unwrap().description(),
        "catalog version prepared",
        "catalog drift must not mutate the already prepared turn view"
    );
}

#[tokio::test]
async fn execute_tool_does_not_inject_idempotency_key_into_strict_tool_args() {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictArgs {}

    struct StrictTool;

    #[async_trait::async_trait]
    impl Tool for StrictTool {
        fn name(&self) -> String {
            "strict_args".into()
        }
        fn description(&self) -> String {
            "test tool with a strict serde contract".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "additionalProperties": false
            })
        }
        async fn execute(&self, input: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            let _: StrictArgs = serde_json::from_value(input)?;
            Ok(ToolResult::ok(json!({ "ok": true })))
        }
    }

    let mgr = ToolsManager::new();
    mgr.registry().register(Arc::new(StrictTool)).await.unwrap();
    let result = mgr
        .execute_tool_with_step(
            None,
            "strict_args",
            json!({}),
            CancellationToken::new(),
            Some("step-0123456789abcdef0123456789abcdef"),
        )
        .await
        .expect("strict tool must execute");
    assert!(result.success, "strict tool failed: {:?}", result.error);
}

#[tokio::test]
async fn test_tools_manager_set_tool_settings() {
    let mgr = ToolsManager::new();
    let mut settings = HashMap::new();
    settings.insert("test_tool".into(), ToolConfig::default());
    mgr.set_tool_settings(settings).await;
}

#[tokio::test]
async fn test_tools_manager_set_context_limits_stores_global_cap() {
    let mgr = ToolsManager::new();
    assert_eq!(
        mgr.core.context_limits.read().await.max_observation_chars,
        16_000
    );
    let limits = ContextLimitsConfig {
        max_observation_chars: 5_000,
        ..Default::default()
    };
    mgr.set_context_limits(limits).await;
    assert_eq!(
        mgr.core.context_limits.read().await.max_observation_chars,
        5_000
    );
}

#[tokio::test]
async fn observation_text_uses_same_global_cap_for_adapters() {
    let mgr = ToolsManager::new();
    let mut limits = ContextLimitsConfig::default();
    limits.max_observation_chars = 4;
    mgr.set_context_limits(limits).await;
    let result = ToolResult::ok(json!("123456"));
    assert_eq!(mgr.observation_text("adapter", &result).await, "1234");
}

#[tokio::test]
async fn test_tools_manager_get_tool_not_found() {
    let mgr = ToolsManager::new();
    let tool = mgr.get_tool("nonexistent").await;
    assert!(tool.is_none());
}

/// Tools that emit a side-channel signal (`ask` / `notify`) must populate
/// `ToolResult::signals` through their `signals()` hook — the ReAct loop
/// reads structured signals instead of name-matching the output. This
/// exercises the full wiring (`execute_tool` → `tool.signals`), so a tool
/// that stops declaring its signal fails here instead of silently losing
/// the ask/notify behavior.
#[tokio::test]
async fn test_signal_declaring_tools_populate_result_signals() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    let ask = mgr
        .execute_tool(
            None,
            "ask",
            json!({"question": "Which file?"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(ask.signals.ask_question.as_deref(), Some("Which file?"));

    let notify = mgr
        .execute_tool(
            None,
            "notify",
            json!({"title": "Build", "body": "Compilation finished"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(notify.signals.notify_title.as_deref(), Some("Build"));
    assert_eq!(
        notify.signals.notify_body.as_deref(),
        Some("Compilation finished")
    );
}

#[tokio::test]
async fn test_tools_manager_execute_tool_not_found() {
    let mgr = ToolsManager::new();
    let result = mgr
        .execute_tool(None, "nonexistent", json!({}), CancellationToken::new())
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_tools_manager_rebuild_catalog_registers_builtins() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    let builtin_tools = mgr.list_builtin_tools().await;
    let names: Vec<_> = builtin_tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
        .collect();
    let unique_names: std::collections::HashSet<_> = names.iter().copied().collect();
    assert_eq!(
        names.len(),
        unique_names.len(),
        "builtin tool names must be unique"
    );

    for (name, group) in [
        ("ask", "haven"),
        ("notify", "system"),
        ("shell", "system"),
        ("http", "system"),
        ("files.read", "system"),
        ("agent.list", "agent"),
    ] {
        let listed = builtin_tools
            .iter()
            .find(|tool| tool["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("builtin tool {name} should be listed"));
        assert_eq!(listed["catalog_group"].as_str(), Some(group), "{name}");
        assert_eq!(
            listed["manifest"]["identity"]["stable_name"].as_str(),
            Some(name),
            "manifest identity drift for {name}"
        );
        assert_eq!(
            listed["manifest"]["identity"]["root"].as_str(),
            Some(name.split('.').next().unwrap_or(name)),
            "manifest root drift for {name}"
        );
        assert!(listed["manifest"]["presentation"]["renderer"].is_string());
    }

    assert!(mgr.get_tool("files").await.is_none());
    assert!(mgr.get_tool("media").await.is_none());
    assert!(mgr.get_tool("audio").await.is_none());
    for name in [
        "files.read",
        "files.outline",
        "files.summary",
        "files.search",
        "system.info",
        "files.write",
        "files.list",
        "process.list",
        "clipboard.read",
        "input.click",
        "window.list",
        "media.inspect",
        "actions.list",
        "schedule.list",
        "preferences.get",
        "checklist.list",
        "agent.list",
    ] {
        let view = mgr.get_tool(name).await;
        assert!(view.is_some(), "operation view {name} should be registered");
        assert!(
            view.unwrap().input_schema()["properties"]
                .get("operation")
                .is_none(),
            "operation discriminator stays fixed in {name}"
        );
    }
    let read_view = mgr.get_tool("files.read").await.unwrap();
    assert!(
        read_view
            .validate_input(&json!({"path": "notes.md"}))
            .is_ok()
    );
    assert!(
        read_view
            .validate_input(&json!({"path": "notes.md", "operation": "write"}))
            .is_err()
    );
    assert_eq!(
        read_view.authorization_input(&json!({"operation": "delete", "path": "notes.md"}))["operation"],
        "read"
    );
    let search_view = mgr.get_tool("files.search").await.unwrap();
    assert_eq!(
        search_view.risk_level(&json!({"mode": "filename"})),
        haven_common::types::RiskLevel::Low
    );
    assert_eq!(
        search_view.risk_level(&json!({"mode": "content"})),
        haven_common::types::RiskLevel::Medium
    );

    assert!(mgr.get_tool("system").await.is_none());
    assert!(mgr.get_tool("process").await.is_none());
    assert!(mgr.get_tool("clipboard").await.is_none());
    assert!(mgr.get_tool("system.env.get").await.is_some());
    assert!(mgr.get_tool("system.power.hibernate").await.is_some());
    assert_eq!(
        mgr.get_tool("process.kill")
            .await
            .expect("process.kill view")
            .risk_level(&json!({})),
        haven_common::types::RiskLevel::High
    );
    assert!(mgr.get_tool("haven").await.is_none());
    assert!(mgr.get_tool("tool_catalog").await.is_some());
    assert!(mgr.get_tool("load_skill").await.is_some());
    assert!(mgr.get_tool("load_mcp").await.is_some());
}

#[tokio::test]
async fn stable_core_tools_are_registered_without_optional_providers() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    for name in CORE_MODEL_TOOLS {
        assert!(
            mgr.registry().get(name).await.is_some(),
            "stable core tool {name} must be provider-visible without optional providers"
        );
    }
    assert!(
        mgr.registry()
            .list()
            .await
            .iter()
            .all(|tool| is_core_model_tool(&tool.name())),
        "default rebuild must keep deferred builtins out of the global provider registry"
    );
}

#[tokio::test]
async fn test_tool_catalog_exposes_three_layers_without_loading_deferred_tools() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;
    let session_id = "ses-0123456789abcdef0123456789abcdef";

    let families = mgr
        .execute_tool(
            Some(session_id),
            "tool_catalog",
            json!({"action": "list"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let family_items = families.output["items"].as_array().unwrap();
    for family in ["agent", "haven", "system"] {
        assert!(
            family_items.iter().any(|item| item["name"] == family),
            "layer 1 should expose the {family} family"
        );
    }
    assert!(
        family_items
            .iter()
            .all(|item| item.get("input_schema").is_none())
    );

    let roots = mgr
        .execute_tool(
            Some(session_id),
            "tool_catalog",
            json!({"action": "list", "level": "tools"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let window = roots.output["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "window")
        .expect("layer 2 should expose the window root");
    assert!(window["operation_count"].as_u64().unwrap() >= 10);
    assert!(window.get("input_schema").is_none());
    assert!(
        mgr.list_defs_for_session(session_id)
            .await
            .iter()
            .all(|def| def.name != "window.screenshot")
    );

    let window_detail = mgr
        .execute_tool(
            Some(session_id),
            "tool_catalog",
            json!({"action": "describe", "name": "window"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let operations = window_detail.output["operations"].as_array().unwrap();
    assert!(
        operations
            .iter()
            .any(|item| item["name"] == "window.screenshot")
    );
    assert!(
        operations
            .iter()
            .all(|item| item.get("input_schema").is_none())
    );

    let operation_page = mgr
        .execute_tool(
            Some(session_id),
            "tool_catalog",
            json!({
                "action": "list",
                "level": "operations",
                "root": "window",
                "limit": 1
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(operation_page.output["items"].as_array().unwrap().len(), 1);
    assert_eq!(operation_page.output["next_cursor"], 1);
    let revision = operation_page.output["catalog_revision"]
        .as_str()
        .expect("paged catalog responses carry a revision")
        .to_string();
    let next_page = mgr
        .execute_tool(
            Some(session_id),
            "tool_catalog",
            json!({
                "action": "list",
                "level": "operations",
                "root": "window",
                "cursor": 1,
                "revision": revision,
                "limit": 1
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(next_page.output["status"], "ok");

    // A config mutation invalidates an outstanding cursor instead of
    // returning a page from a different catalog snapshot.
    mgr.upsert_mcp_server_config(McpServerConfig {
        name: "catalog-revision-test".into(),
        enabled: true,
        ..Default::default()
    })
    .await;
    let stale_page = mgr
        .execute_tool(
            Some(session_id),
            "tool_catalog",
            json!({
                "action": "list",
                "level": "operations",
                "root": "window",
                "cursor": 1,
                "revision": operation_page.output["catalog_revision"],
                "limit": 1
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(stale_page.output["status"], "stale_cursor");

    let screenshot_detail = mgr
        .execute_tool(
            Some(session_id),
            "tool_catalog",
            json!({"action": "describe", "name": "window.screenshot"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(screenshot_detail.output["loaded"], false);
    assert!(screenshot_detail.output["input_schema"].is_object());
    assert_eq!(screenshot_detail.output["load"]["tool"], "tool_catalog");

    let loaded = mgr
        .execute_tool(
            Some(session_id),
            "tool_catalog",
            json!({
                "action": "load",
                "source": "builtin",
                "operations": ["window.screenshot"]
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(loaded.success);
    assert!(
        mgr.get_tool_for_session(Some(session_id), "window.screenshot")
            .await
            .is_some()
    );
}

#[tokio::test]
async fn tool_catalog_describe_does_not_connect_or_execute_mcp() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;
    mgr.upsert_mcp_server_config(McpServerConfig {
        name: "unconnected-catalog-server".into(),
        command: "this-command-must-not-run".into(),
        enabled: true,
        ..Default::default()
    })
    .await;

    let result = mgr
        .execute_tool(
            Some("ses-0123456789abcdef0123456789abcdef"),
            "tool_catalog",
            json!({
                "action": "describe",
                "name": "unconnected-catalog-server"
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.output["status"], "ok");
    assert_eq!(result.output["source"], "mcp");
    assert!(mgr.mcp_manager().list_clients().await.is_empty());
}

#[tokio::test]
async fn test_tools_manager_disabled_tool_excluded_and_blocked() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    // Disable the `files` tool.
    let mut settings = HashMap::new();
    settings.insert(
        "files".into(),
        ToolConfig {
            enabled: false,
            ..Default::default()
        },
    );
    mgr.set_tool_settings(settings).await;

    // Excluded from the agent-facing registry...
    assert!(mgr.get_tool("files").await.is_none());
    let schemas = mgr.registry().list_schemas().await;
    assert!(!schemas.iter().any(|s| s["name"].as_str() == Some("files")));
    assert!(mgr.get_tool("files.read").await.is_none());
    assert!(mgr.get_tool("files.search").await.is_none());

    // ...still listed for the UI with enabled = false...
    let all = mgr.list_builtin_tools().await;
    let file = all
        .iter()
        .find(|s| s["name"].as_str() == Some("files.read"))
        .unwrap();
    assert_eq!(file["enabled"].as_bool(), Some(false));

    // ...and execution is blocked.
    let result = mgr
        .execute_tool(
            None,
            "files.read",
            json!({"path": "notes.md"}),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("disabled"));
}

#[tokio::test]
async fn test_tools_manager_execute_builtin_tool() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("hello.txt");
    tokio::fs::write(&file, "hello from manager").await.unwrap();

    let result = mgr
        .execute_tool(
            None,
            "files.read",
            json!({"path": file.to_string_lossy()}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(
        result.output["content"].as_str().unwrap(),
        "hello from manager"
    );
}

#[tokio::test]
async fn operation_view_accepts_trusted_private_session_metadata() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("listed.txt");
    tokio::fs::write(&file, "listed by manager").await.unwrap();
    assert!(
        mgr.load_builtin_operations_for_session(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some(vec!["files.list".into()]),
            None,
        )
        .await
    );

    let result = mgr
        .execute_tool(
            Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "files.list",
            json!({"path": tmp.path().to_string_lossy()}),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(result.success, "files.list failed: {:?}", result.error);
    assert_eq!(result.output["count"], 1);
}

#[tokio::test]
async fn test_tools_manager_get_risk_level_unknown() {
    let mgr = ToolsManager::new();
    let risk = mgr.get_risk_level(None, "nonexistent", &json!({})).await;
    assert_eq!(risk, RiskLevel::Safe);
}

/// End-to-end: execute_tool fast-fails once the per-tool circuit opens
/// (refine §5).
#[tokio::test]
async fn test_execute_tool_circuit_breaker_opens() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FailingTool {
        name: String,
        call_count: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> String {
            self.name.clone()
        }
        fn description(&self) -> String {
            "always fails".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(
            &self,
            _: Value,
            _: tokio_util::sync::CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("deliberate failure")
        }
    }

    let mgr = ToolsManager::new();
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.registry()
        .register(Arc::new(FailingTool {
            name: "failing".into(),
            call_count: call_count.clone(),
        }))
        .await
        .unwrap();

    for i in 0..5 {
        let r = mgr
            .execute_tool(None, "failing", json!({}), CancellationToken::new())
            .await;
        assert!(!r.unwrap().success, "call {} should fail", i + 1);
    }
    assert!(mgr.tool_circuits().is_open("failing"));

    let before = call_count.load(Ordering::SeqCst);
    let r = mgr
        .execute_tool(None, "failing", json!({}), CancellationToken::new())
        .await;
    assert!(r.is_err());
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        before,
        "tool should not be called when breaker is open"
    );
    assert!(
        r.unwrap_err().to_string().contains("circuit breaker"),
        "error should mention circuit breaker"
    );
}

#[tokio::test]
async fn execute_tool_retries_transient_failure_by_default() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FlakyTool {
        attempts: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl Tool for FlakyTool {
        fn name(&self) -> String {
            "flaky".into()
        }
        fn description(&self) -> String {
            "fails once with a transient error".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn idempotency(&self, _: &Value) -> OperationIdempotency {
            OperationIdempotency::Idempotent
        }
        fn error_metadata(&self, _: &anyhow::Error) -> ToolErrorMetadata {
            ToolErrorMetadata::transient()
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(
            &self,
            _: Value,
            _: tokio_util::sync::CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("service unavailable")
            }
            Ok(ToolResult::ok(json!({"recovered": true})))
        }
    }

    let mgr = ToolsManager::new();
    let attempts = Arc::new(AtomicU32::new(0));
    mgr.set_tool_settings(HashMap::from([(
        "flaky".into(),
        ToolConfig {
            max_retries: Some(1),
            retry_backoff_secs: Some(0),
            ..Default::default()
        },
    )]))
    .await;
    mgr.registry()
        .register(Arc::new(FlakyTool {
            attempts: attempts.clone(),
        }))
        .await
        .unwrap();

    let result = mgr
        .execute_tool(None, "flaky", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn execute_tool_never_retries_when_side_effect_outcome_is_unknown() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct UnknownSideEffectTool {
        calls: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl Tool for UnknownSideEffectTool {
        fn name(&self) -> String {
            "unknown_side_effect".into()
        }
        fn description(&self) -> String {
            "test tool with an unknown side-effect outcome".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::High
        }
        fn idempotency(&self, _: &Value) -> OperationIdempotency {
            OperationIdempotency::Idempotent
        }
        fn default_max_retries(&self) -> u32 {
            3
        }
        fn default_retry_backoff_secs(&self) -> u64 {
            0
        }
        fn error_metadata(&self, _: &anyhow::Error) -> ToolErrorMetadata {
            ToolErrorMetadata {
                class: ToolErrorClass::SideEffectMayHaveHappened,
                outcome: ToolExecutionOutcome::Failed,
                retryability: ToolRetryability::Unknown,
            }
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object", "additionalProperties": false})
        }
        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::Error::new(StructuredToolError::new(
                "the external side effect may have happened",
                ToolErrorMetadata {
                    class: ToolErrorClass::SideEffectMayHaveHappened,
                    outcome: ToolExecutionOutcome::Failed,
                    retryability: ToolRetryability::Unknown,
                },
            )))
        }
    }

    let calls = Arc::new(AtomicU32::new(0));
    let manager = ToolsManager::new();
    manager
        .set_tool_settings(HashMap::from([(
            "unknown_side_effect".into(),
            ToolConfig {
                max_retries: Some(3),
                retry_backoff_secs: Some(0),
                ..Default::default()
            },
        )]))
        .await;
    manager
        .registry()
        .register(Arc::new(UnknownSideEffectTool {
            calls: calls.clone(),
        }))
        .await
        .unwrap();

    let result = manager
        .execute_tool(
            None,
            "unknown_side_effect",
            json!({}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!result.success);
    assert_eq!(result.attempts, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        result.error_class,
        Some(ToolErrorClass::SideEffectMayHaveHappened)
    );
    assert_eq!(result.retryability, ToolRetryability::Unknown);
}

#[tokio::test]
async fn settings_without_timeout_preserve_intrinsic_timeout() {
    struct SlowIntrinsicTool;

    #[async_trait::async_trait]
    impl Tool for SlowIntrinsicTool {
        fn name(&self) -> String {
            "slow_intrinsic".into()
        }
        fn description(&self) -> String {
            "test tool".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::High
        }
        fn default_timeout_secs(&self) -> u64 {
            1
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(ToolResult::ok(json!({"done": true})))
        }
    }

    let mgr = ToolsManager::new();
    mgr.set_tool_settings(HashMap::from([(
        "slow_intrinsic".into(),
        ToolConfig {
            max_output_chars: Some(100),
            ..Default::default()
        },
    )]))
    .await;
    mgr.registry()
        .register(Arc::new(SlowIntrinsicTool))
        .await
        .unwrap();

    let result = mgr
        .execute_tool(None, "slow_intrinsic", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.outcome, ToolExecutionOutcome::TimedOutUnknown);
    assert_eq!(result.attempts, 1);
}

#[tokio::test]
async fn settings_without_retry_fields_preserve_intrinsic_retry_policy() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct IntrinsicRetryTool {
        attempts: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl Tool for IntrinsicRetryTool {
        fn name(&self) -> String {
            "intrinsic_retry".into()
        }
        fn description(&self) -> String {
            "test tool".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn idempotency(&self, _: &Value) -> OperationIdempotency {
            OperationIdempotency::Idempotent
        }
        fn default_max_retries(&self) -> u32 {
            1
        }
        fn default_retry_backoff_secs(&self) -> u64 {
            0
        }
        fn error_metadata(&self, _: &anyhow::Error) -> ToolErrorMetadata {
            ToolErrorMetadata::transient()
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("service unavailable")
            }
            Ok(ToolResult::ok(json!({"recovered": true})))
        }
    }

    let mgr = ToolsManager::new();
    let attempts = Arc::new(AtomicU32::new(0));
    mgr.set_tool_settings(HashMap::from([(
        "intrinsic_retry".into(),
        ToolConfig {
            max_output_chars: Some(100),
            ..Default::default()
        },
    )]))
    .await;
    mgr.registry()
        .register(Arc::new(IntrinsicRetryTool {
            attempts: attempts.clone(),
        }))
        .await
        .unwrap();

    let result = mgr
        .execute_tool(None, "intrinsic_retry", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.attempts, 2);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn retryable_tool_results_require_known_transient_failure() {
    assert!(retryable_result(&ToolResult::failed_with_class(
        Value::Null,
        "status 503",
        ToolErrorClass::Transient,
    )));
    assert!(!retryable_result(&ToolResult::failed(
        Value::Null,
        "connection refused",
    )));
    assert!(!retryable_result(&ToolResult::cancelled(
        "cancelled while waiting"
    )));
    assert!(!retryable_result(&ToolResult::timed_out(
        ToolExecutionOutcome::TimedOutUnknown,
        "timeout"
    )));
}

// ── Progressive loading: per-session schemas & MCP index ──────────────

#[tokio::test]
async fn test_list_schemas_for_session_includes_per_session_tools() {
    use haven_skills::SkillManifest;

    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    // Before registering a per-session tool, schemas come only from the
    // global registry.
    let base_schemas = mgr.list_schemas_for_session("ses-a").await;
    let base_count = base_schemas.len();

    // Register a fake per-session tool.
    let manifest = SkillManifest {
        name: "demo".into(),
        description: "demo skill".into(),
        version: None,
        language: haven_skills::Language::Python,
        instructions: "do stuff".into(),
    };
    let skill = Skill::from_manifest_unchecked(manifest, std::path::PathBuf::from("."), true);
    let runner = mgr.skill_runner().read().await.clone();
    let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
    mgr.register_for_session("ses-a", Arc::new(adapter)).await;

    let schemas = mgr.list_schemas_for_session("ses-a").await;
    assert_eq!(
        schemas.len(),
        base_count + 1,
        "per-session skill tool should appear in schemas"
    );
    assert!(schemas.iter().any(|s| s["name"] == "skill__demo"));

    // Other sessions should NOT see this tool.
    let other = mgr.list_schemas_for_session("ses-b").await;
    assert_eq!(other.len(), base_count);
    assert!(!other.iter().any(|s| s["name"] == "skill__demo"));
}

#[tokio::test]
async fn test_session_catalog_version_does_not_invalidate_other_sessions() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;
    let before_a = mgr.catalog_version_for_session("ses-a").await;
    let before_b = mgr.catalog_version_for_session("ses-b").await;

    struct NamedStub(&'static str);
    #[async_trait::async_trait]
    impl Tool for NamedStub {
        fn name(&self) -> String {
            self.0.into()
        }
        fn description(&self) -> String {
            "stub".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({})))
        }
    }

    mgr.register_for_session("ses-a", Arc::new(NamedStub("session_only")))
        .await;
    let after_a = mgr.catalog_version_for_session("ses-a").await;
    let after_b = mgr.catalog_version_for_session("ses-b").await;
    assert_eq!(after_a.0, before_a.0);
    assert_eq!(after_a.1, before_a.1 + 1);
    assert_eq!(after_b, before_b);

    mgr.rebuild_catalog().await;
    let after_global_a = mgr.catalog_version_for_session("ses-a").await;
    let after_global_b = mgr.catalog_version_for_session("ses-b").await;
    assert!(after_global_a.0 > after_a.0);
    assert_eq!(after_global_a.1, after_a.1);
    assert_eq!(after_global_b.1, after_b.1);
}

#[tokio::test]
async fn test_build_mcp_index_filters_disabled() {
    use haven_common::config::McpServerConfig;

    let mgr = ToolsManager::new();
    mgr.upsert_mcp_server_config(McpServerConfig {
        name: "on".into(),
        enabled: true,
        ..Default::default()
    })
    .await;
    mgr.upsert_mcp_server_config(McpServerConfig {
        name: "off".into(),
        enabled: false,
        ..Default::default()
    })
    .await;

    let index = mgr.build_mcp_index().await;
    let names: Vec<&str> = index.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(names.contains(&"on"));
    assert!(!names.contains(&"off"), "disabled server should not appear");
}

#[test]
fn mcp_search_detection_only_uses_cached_tool_names() {
    assert!(mcp_index_entry_has_search_tool(&serde_json::json!({
        "name": "research",
        "description": "MCP server 'research'; tools: fetch, web_search",
    })));
    assert!(!mcp_index_entry_has_search_tool(&serde_json::json!({
        "name": "search-like-server",
        "description": "MCP server 'search-like-server'",
    })));
}

#[tokio::test]
async fn test_build_mcp_index_does_not_expose_process_args() {
    use haven_common::config::McpServerConfig;

    let mgr = ToolsManager::new();
    mgr.upsert_mcp_server_config(McpServerConfig {
        name: "safe-server".into(),
        command: "server.exe".into(),
        args: vec!["--token".into(), "SECRET_SHOULD_NOT_REACH_PROMPT".into()],
        enabled: true,
        ..Default::default()
    })
    .await;

    let index = mgr.build_mcp_index().await;
    let description = index[0]["description"].as_str().unwrap_or("");
    assert!(!description.contains("server.exe"));
    assert!(!description.contains("SECRET_SHOULD_NOT_REACH_PROMPT"));
    assert!(description.contains("safe-server"));
}

#[tokio::test]
async fn test_upsert_and_remove_mcp_server_config() {
    use haven_common::config::McpServerConfig;

    let mgr = ToolsManager::new();
    mgr.upsert_mcp_server_config(McpServerConfig {
        name: "srv".into(),
        enabled: true,
        ..Default::default()
    })
    .await;
    assert_eq!(mgr.build_mcp_index().await.len(), 1);

    mgr.remove_mcp_server_config("srv").await;
    assert!(mgr.build_mcp_index().await.is_empty());
}

#[tokio::test]
async fn test_rebuild_catalog_does_not_register_mcp_tools() {
    // Progressive loading: MCP tools must NOT be in the global registry.
    // They should only appear per-session after `load_mcp`.
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;
    let schemas = mgr.registry().list_schemas().await;
    assert!(
        !schemas
            .iter()
            .any(|s| { s["name"].as_str().unwrap_or("").starts_with("mcp__") }),
        "MCP tools must not be pre-registered globally"
    );
}

#[tokio::test]
async fn test_builtin_loader_keeps_deferred_tools_out_of_provider_surface() {
    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;

    assert!(mgr.registry().get("shell").await.is_none());
    assert!(mgr.get_tool("shell").await.is_some());
    assert!(
        mgr.list_defs_for_session("ses-lazy-builtin")
            .await
            .iter()
            .all(|def| def.name != "shell")
    );

    let result = mgr
        .execute_tool(
            Some("ses-lazy-builtin"),
            "tool_catalog",
            serde_json::json!({
                "action": "load",
                "source": "builtin",
                "operations": ["shell"]
            }),
            CancellationToken::new(),
        )
        .await
        .expect("tool_catalog load should be executable from the core surface");
    assert!(result.success, "loader failed: {:?}", result.error);
    assert_eq!(result.output["status"], "loaded");
    assert!(result.output.get("input_schema").is_none());

    let loaded = mgr.list_defs_for_session("ses-lazy-builtin").await;
    assert!(loaded.iter().any(|def| def.name == "shell"));
}

#[tokio::test]
async fn test_list_defs_for_session_caps_at_max_tools() {
    use haven_common::config::ContextLimitsConfig;

    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;
    let global = mgr.registry().list_defs().await.len();
    assert!(global > 0, "catalog should have builtins");

    // Leave room for only 2 session overlays. Write the limit directly so
    // we do not rebuild the catalog (and shift `global`) mid-test.
    let mut limits = ContextLimitsConfig::default();
    limits.max_tools_per_request = global + 2;
    *mgr.core.context_limits.write().await = limits;

    struct NamedStub(&'static str);
    #[async_trait::async_trait]
    impl Tool for NamedStub {
        fn name(&self) -> String {
            self.0.into()
        }
        fn description(&self) -> String {
            "stub".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({})))
        }
    }
    for name in ["s_a", "s_b", "s_c", "s_d", "s_e"] {
        mgr.register_for_session("ses-cap", Arc::new(NamedStub(name)))
            .await;
    }

    let defs = mgr.list_defs_for_session("ses-cap").await;
    assert_eq!(defs.len(), global + 2, "must truncate session overlays");
    let session_kept: Vec<_> = defs
        .iter()
        .filter(|d| d.name.starts_with("s_"))
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(session_kept, vec!["s_a", "s_b"]);
}

/// LLM-/schedule-supplied `_step_id` / `_session_id` must never reach the
/// live-output hub. Without a trusted step id the shell path stays silent;
/// with one, only the trusted id is emitted.
#[cfg(windows)]
#[tokio::test]
async fn private_live_output_ids_are_stripped_and_reinjected() {
    use std::sync::Mutex;

    let mgr = ToolsManager::new();
    mgr.rebuild_catalog().await;
    let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let hits2 = hits.clone();
    mgr.live_outputs()
        .set_event_sink(Arc::new(move |_event, payload| {
            if let Some(sid) = payload["step_id"].as_str() {
                hits2.lock().unwrap().push(sid.to_string());
            }
        }));

    // Forged private fields, no trusted step → no live emit.
    let _ = mgr
        .execute_tool(
            Some("ses-1"),
            "shell",
            json!({
                "command": "echo forged",
                "shell": "cmd",
                "_step_id": "step-forged",
                "_session_id": "ses-evil",
            }),
            CancellationToken::new(),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !hits.lock().unwrap().iter().any(|s| s == "step-forged"),
        "forged step id must not reach agent:tool_output"
    );

    // Trusted step id wins over a forged one in the input.
    hits.lock().unwrap().clear();
    let _ = mgr
        .execute_tool_with_step(
            Some("ses-1"),
            "shell",
            json!({
                "command": "echo trusted",
                "shell": "cmd",
                "_step_id": "step-forged",
            }),
            CancellationToken::new(),
            Some("step-trusted"),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let seen = hits.lock().unwrap().clone();
    assert!(
        !seen.iter().any(|s| s == "step-forged"),
        "forged id must be overwritten by trusted step id"
    );
    // Live emit is best-effort (fast commands may finish before the first
    // tick); when anything is emitted it must be the trusted id.
    assert!(
        seen.iter().all(|s| s == "step-trusted"),
        "only trusted step id may be emitted, got {seen:?}"
    );
}
