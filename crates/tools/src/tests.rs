use super::*;
use haven_common::tools::{
    ToolAvailability, ToolConcurrencyMode, ToolIdentity, ToolModel, ToolPolicy, ToolPresentation,
};
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
            confirmation: ConfirmationRequirement::None,
            idempotency: OperationIdempotency::Unknown,
            scope: ToolOperationScope::Session,
            concurrency: ToolConcurrencyMode::Exclusive,
            effect: OperationEffect::ReadOnly,
            data_sensitivity: DataSensitivity::None,
            network_access: NetworkAccess::None,
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
    let selection = select_tool_definitions_for_budget(
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

    let selected_tools: Vec<_> = selection
        .selected_tools
        .iter()
        .map(|def| def.name.as_str())
        .collect();
    assert_eq!(selected_tools, ["core.a", "core.z", "mcp__a", "mcp__z"]);
    assert_eq!(selection.omitted, ["skill__a", "skill__z"]);
    assert_eq!(selection.omitted_core, 0);
}

#[test]
fn tool_budget_selection_reports_core_omissions_when_core_exceeds_limit() {
    let selection = select_tool_definitions_for_budget(
        vec![
            budget_test_def("core.c", ToolSource::Builtin),
            budget_test_def("skill__a", ToolSource::Skill),
            budget_test_def("core.a", ToolSource::Builtin),
            budget_test_def("core.b", ToolSource::Builtin),
        ],
        Vec::new(),
        2,
    );

    let selected_tools: Vec<_> = selection
        .selected_tools
        .iter()
        .map(|def| def.name.as_str())
        .collect();
    assert_eq!(selected_tools, ["core.a", "core.b"]);
    assert_eq!(selection.omitted, ["core.c", "skill__a"]);
    assert_eq!(selection.omitted_core, 1);
}

#[tokio::test]
async fn test_tools_facade_new() {
    let mgr = ToolsFacade::new();
    let tools = mgr.registry().list().await;
    assert!(tools.is_empty());
}

#[tokio::test]
async fn scoped_catalog_rebuild_reuses_unaffected_runtime_instances() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let ask_before = mgr.registry().get("ask").await.unwrap();
    let shell_before = mgr.get_tool("shell").await.unwrap();

    let outcome = mgr.set_default_shell(ShellChoice::default()).await.unwrap();
    assert_eq!(outcome, CatalogRebuildOutcome::Published);

    let ask_after = mgr.registry().get("ask").await.unwrap();
    let shell_after = mgr.get_tool("shell").await.unwrap();
    assert!(Arc::ptr_eq(&ask_before, &ask_after));
    assert!(!Arc::ptr_eq(&shell_before, &shell_after));
}

#[tokio::test]
async fn manager_control_plane_errors_preserve_structured_metadata() {
    let mgr = ToolsFacade::new();
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
    .await
    .unwrap();
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
    let mgr = ToolsFacade::new();
    let capabilities = mgr.runtime_capabilities().await;
    assert!(!capabilities.vision);
    assert!(!capabilities.image_generation);
    assert!(!capabilities.transcription);
    assert!(!capabilities.recording);
    assert!(!capabilities.tts);
    assert_eq!(capabilities.web_search, WebSearchAvailability::Unavailable);
}

#[tokio::test]
async fn tool_catalog_snapshot_captures_lookup_policy_and_manifest() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();

    let snapshot = mgr
        .tool_catalog_snapshot("ses-7d496d1fd2a974678388198ef1c7dd0a")
        .await;
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

struct SessionAuthorizationTool;

#[async_trait::async_trait]
impl Tool for SessionAuthorizationTool {
    fn name(&self) -> String {
        "mcp__policy__invoke".into()
    }

    fn description(&self) -> String {
        "session authorization policy probe".into()
    }

    fn risk_level(&self, _: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn authorization_input(&self, input: &Value) -> Value {
        let mut canonical = input.as_object().cloned().unwrap_or_default();
        canonical.insert("operation".into(), json!("invoke"));
        Value::Object(canonical)
    }

    async fn authorization_target_binding(&self, _: &Value) -> Result<Option<String>, String> {
        Ok(Some("mcp-target:0000000000000001:0000000000000002".into()))
    }

    async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::ok(json!({"ok": true})))
    }
}

#[tokio::test]
async fn lsp_live_and_snapshot_requests_declare_project_read_scope() {
    let root = TempDir::new().unwrap();
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    mgr.switch_execution_workspace(root.path()).await.unwrap();
    let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let tool = crate::builtin::lsp::LspTool::new(
        Default::default(),
        Arc::clone(&mgr.coordinator.core.authorization),
    );
    mgr.register_for_session(session_id, Arc::new(tool)).await;
    let input = json!({"operation": "workspace_symbols", "server": "test", "query": "x"});
    let catalog = mgr.tool_catalog_snapshot(session_id).await;
    let live = mgr
        .resolve_authorization_request(Some(session_id), "lsp", &input)
        .await
        .unwrap();
    let snapshot = mgr
        .resolve_authorization_request_from_snapshot(&catalog, Some(session_id), "lsp", &input)
        .await
        .unwrap();
    assert_eq!(live.resources, snapshot.resources);
    assert!(live.resources.iter().any(|resource| matches!(resource, DeclaredResource::LocalPath { path, access: LocalPathAccess::Read } if path == root.path())));
}

#[tokio::test]
async fn git_live_and_snapshot_requests_include_linked_worktree_resources() {
    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    let worktree = root.path().join("worktree");
    let common = root.path().join("repo/.git");
    let metadata = common.join("worktrees/linked");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::create_dir_all(&metadata).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", metadata.display()),
    )
    .unwrap();
    std::fs::write(metadata.join("commondir"), "../..\n").unwrap();
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    mgr.switch_execution_workspace(&workspace).await.unwrap();
    let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let tool =
        crate::builtin::git::GitTool::new(Arc::clone(&mgr.coordinator.core.authorization), 8000);
    mgr.register_for_session(session_id, Arc::new(tool)).await;
    let input = json!({"subcommand":"status", "cwd":worktree.join("src")});
    let catalog = mgr.tool_catalog_snapshot(session_id).await;
    let live = mgr
        .resolve_authorization_request(Some(session_id), "git", &input)
        .await
        .unwrap();
    let snapshot = mgr
        .resolve_authorization_request_from_snapshot(&catalog, Some(session_id), "git", &input)
        .await
        .unwrap();
    assert_eq!(live.resources, snapshot.resources);
    for path in [worktree.join("src"), worktree, metadata, common] {
        assert!(live.resources.iter().any(|resource| matches!(resource, DeclaredResource::LocalPath { path: actual, access: LocalPathAccess::Write } if actual == &path)));
    }
}

#[tokio::test]
async fn authorization_policy_matches_live_and_snapshot_session_overlay() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let tool_name = "mcp__policy__invoke";
    let input = json!({"value": "payload"});
    mgr.register_for_session(session_id, Arc::new(SessionAuthorizationTool))
        .await;

    let catalog = mgr.tool_catalog_snapshot(session_id).await;
    let live = mgr
        .resolve_authorization_request(Some(session_id), tool_name, &input)
        .await
        .unwrap();
    let snapshot = mgr
        .resolve_authorization_request_from_snapshot(&catalog, Some(session_id), tool_name, &input)
        .await
        .unwrap();

    assert_eq!(live.session_id.as_deref(), Some(session_id));
    assert_eq!(live.tool_name, tool_name);
    assert_eq!(
        live.input,
        json!({"value": "payload", "operation": "invoke"})
    );
    assert_eq!(live.policy.risk_level, RiskLevel::High);
    assert_eq!(snapshot.session_id, live.session_id);
    assert_eq!(snapshot.tool_name, live.tool_name);
    assert_eq!(snapshot.input, live.input);
    assert_eq!(snapshot.policy, live.policy);
    assert_eq!(snapshot.target_binding, live.target_binding);
    assert_eq!(
        live.target_binding.as_deref(),
        Some("mcp-target:0000000000000001:0000000000000002")
    );

    let missing_name = "mcp__missing__invoke";
    let live_unknown = mgr
        .resolve_authorization_request(Some(session_id), missing_name, &input)
        .await
        .unwrap();
    let snapshot_unknown = mgr
        .resolve_authorization_request_from_snapshot(
            &catalog,
            Some(session_id),
            missing_name,
            &input,
        )
        .await
        .unwrap();
    assert_eq!(snapshot_unknown.session_id, live_unknown.session_id);
    assert_eq!(snapshot_unknown.tool_name, live_unknown.tool_name);
    assert_eq!(snapshot_unknown.input, live_unknown.input);
    assert_eq!(snapshot_unknown.policy, live_unknown.policy);
    assert_eq!(live_unknown.policy.risk_level, RiskLevel::Safe);
}

#[tokio::test]
async fn load_skill_activation_is_in_process_while_skill_execution_stays_host_process() {
    let facade = ToolsFacade::new();
    facade.rebuild_catalog().await.unwrap();
    let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let authorization = facade.share_services().authorization;
    let mut security = haven_common::config::SecurityConfig::default();
    security.resource_policy.network.access = haven_common::config::NetworkAccessBoundary::Deny;
    facade.apply_security(&security).await.unwrap();

    let activation = facade
        .resolve_authorization_request(
            Some(session_id),
            "load_skill",
            &json!({"skill_names": ["demo"]}),
        )
        .await
        .unwrap();
    assert_eq!(activation.policy.network_access, NetworkAccess::None);
    assert_eq!(
        activation.policy.execution_boundary,
        ExecutionBoundary::InProcess
    );
    assert!(matches!(
        authorization
            .authorize_for_execution(&activation)
            .await
            .decision,
        AuthorizationDecision::AutoApproved
    ));

    let skills_root = TempDir::new().unwrap();
    let skill = crate::test_support::discover_skill_fixture(
        skills_root.path(),
        "demo",
        "Sandbox boundary test",
        Some("print('{}')\n"),
    )
    .await;
    let config = haven_common::config::SkillsExecConfig::default();
    let runner = crate::skill_runner::SkillRunner::new(
        haven_skills::VenvManager::new(config.venv_root.clone()),
        config,
    );
    let skill_tool_name = crate::skill_tool_name("demo");
    facade
        .register_for_session(
            session_id,
            Arc::new(SkillToolAdapter::new(Arc::new(skill), runner)),
        )
        .await;
    security.resource_policy.network.access = haven_common::config::NetworkAccessBoundary::Open;
    facade.apply_security(&security).await.unwrap();
    let execution = facade
        .resolve_authorization_request(Some(session_id), &skill_tool_name, &json!({"params": {}}))
        .await
        .unwrap();
    assert_eq!(execution.policy.network_access, NetworkAccess::Opaque);
    assert_eq!(
        execution.policy.execution_boundary,
        ExecutionBoundary::HostProcess
    );
    assert!(matches!(
        authorization
            .authorize_for_execution(&execution)
            .await
            .decision,
        AuthorizationDecision::RequiresConfirmation {
            reason_code: AuthorizationReasonCode::ResourceBoundaryExtension,
            ..
        }
    ));
}

#[tokio::test]
async fn tool_catalog_snapshot_keeps_provider_surface_stable_after_drift() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let before = mgr
        .tool_catalog_snapshot("ses-8c81e0e6d5e4b639d41be6f9e46e25c1")
        .await;
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

    mgr.register_for_session("ses-8c81e0e6d5e4b639d41be6f9e46e25c1", Arc::new(DriftTool))
        .await;
    let after = mgr
        .tool_catalog_snapshot("ses-8c81e0e6d5e4b639d41be6f9e46e25c1")
        .await;
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
    assert_ne!(before.catalog_version(), after.catalog_version());
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

    let mgr = ToolsFacade::new();
    mgr.registry()
        .register(Arc::new(DriftTool("prepared")))
        .await
        .unwrap();
    let catalog = mgr
        .tool_catalog_snapshot("ses-d2cf2d2794bfc431c5fe04d19fbc2de8")
        .await;
    assert_eq!(
        catalog.get("drift_execution").unwrap().description(),
        "catalog version prepared"
    );

    // The prepared provider surface remains immutable, but execution must
    // consult the current session overlay at the safety boundary.
    mgr.register_for_session(
        "ses-d2cf2d2794bfc431c5fe04d19fbc2de8",
        Arc::new(DriftTool("live")),
    )
    .await;
    let result = mgr
        .execute_tool(
            Some("ses-d2cf2d2794bfc431c5fe04d19fbc2de8"),
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

    let mgr = ToolsFacade::new();
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
async fn authorized_execution_rejects_a_revoked_proof_before_calling_the_tool() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CounterTool(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl Tool for CounterTool {
        fn name(&self) -> String {
            "proof_gate_probe".into()
        }
        fn description(&self) -> String {
            "Counts execution attempts for authorization proof tests".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object", "additionalProperties": false})
        }
        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::ok(json!({"executed": true})))
        }
    }

    let tools = ToolsFacade::new();
    let executions = Arc::new(AtomicUsize::new(0));
    tools
        .registry()
        .register(Arc::new(CounterTool(executions.clone())))
        .await
        .unwrap();
    let input = json!({});
    let request = tools
        .resolve_authorization_request(None, "proof_gate_probe", &input)
        .await
        .unwrap();
    let authorization = tools.share_services().authorization;
    let check = authorization.authorize_for_execution(&request).await;
    assert!(matches!(
        check.decision,
        AuthorizationDecision::AutoApproved
    ));
    let proof = check.execution_proof.unwrap();

    let mut security = haven_common::config::SecurityConfig::default();
    security.resource_policy.files.write_mode = haven_common::config::FileWriteMode::Unrestricted;
    security.resource_policy.network.access = haven_common::config::NetworkAccessBoundary::Open;
    tools.apply_security(&security).await.unwrap();
    let result = tools
        .execute_authorized_tool_with_step(
            None,
            "proof_gate_probe",
            input,
            CancellationToken::new(),
            None,
            proof,
        )
        .await
        .unwrap();

    assert!(!result.success);
    assert_eq!(result.error_class, Some(ToolErrorClass::Permission));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_tools_facade_set_tool_settings() {
    let mgr = ToolsFacade::new();
    let mut settings = HashMap::new();
    settings.insert("test_tool".into(), ToolConfig::default());
    mgr.set_tool_settings(settings).await.unwrap();
}

#[tokio::test]
async fn unchanged_tool_settings_report_that_catalog_rebuild_was_skipped() {
    let mgr = ToolsFacade::new();
    let version_before = mgr.registry().version();

    let outcome = mgr.set_tool_settings(HashMap::new()).await.unwrap();

    assert_eq!(outcome, CatalogRebuildOutcome::Unchanged);
    assert_eq!(mgr.registry().version(), version_before);
}

#[tokio::test]
async fn test_tools_facade_set_context_limits_stores_global_cap() {
    let mgr = ToolsFacade::new();
    assert_eq!(mgr.context_limits().await.max_observation_chars, 16_000);
    let limits = ContextLimitsConfig {
        max_observation_chars: 5_000,
        ..Default::default()
    };
    mgr.set_context_limits(limits).await.unwrap();
    assert_eq!(mgr.context_limits().await.max_observation_chars, 5_000);
}

#[tokio::test]
async fn observation_text_uses_same_global_cap_for_adapters() {
    let mgr = ToolsFacade::new();
    let limits = ContextLimitsConfig {
        max_observation_chars: 4,
        ..Default::default()
    };
    mgr.set_context_limits(limits).await.unwrap();
    let result = ToolResult::ok(json!("123456"));
    assert_eq!(mgr.observation_text("adapter", &result).await, "1234");
}

#[tokio::test]
async fn test_tools_facade_get_tool_not_found() {
    let mgr = ToolsFacade::new();
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
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();

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
async fn test_tools_facade_execute_tool_not_found() {
    let mgr = ToolsFacade::new();
    let result = mgr
        .execute_tool(None, "nonexistent", json!({}), CancellationToken::new())
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_tools_facade_rebuild_catalog_registers_builtins() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();

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
        ("ask", "interaction"),
        ("notify", "interaction"),
        ("shell", "shell"),
        ("http", "network"),
        ("files.read", "files"),
        ("clipboard.list_history", "desktop"),
        ("agent.list", "workflow"),
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

    for name in ["discover_tools", "load_skill", "load_mcp"] {
        let listed = builtin_tools
            .iter()
            .find(|tool| tool["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("builtin tool {name} should be listed"));
        assert_eq!(
            listed["catalog_group"].as_str(),
            Some("extensions"),
            "{name}"
        );
    }

    assert!(mgr.get_tool("files").await.is_none());
    assert!(mgr.get_tool("media").await.is_none());
    assert!(mgr.get_tool("audio").await.is_none());
    for name in [
        "files.read",
        "files.get_outline",
        "files.summarize",
        "files.search",
        "system.get_info",
        "files.write",
        "files.list",
        "clipboard.read",
        "input.click",
        "window.list",
        "media.inspect",
        "toolrun.list",
        "schedule.list",
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
    for name in [
        "process.list",
        "process.kill",
        "preferences.get",
        "checklist.list",
        "window.ocr",
        "system.env.set",
        "system.env.unset",
        "system.registry.set",
        "system.registry.delete_value",
        "system.registry.delete_key",
    ] {
        assert!(
            mgr.get_tool(name).await.is_none(),
            "removed operation {name}"
        );
    }
    assert!(mgr.get_tool("haven").await.is_none());
    assert!(mgr.get_tool("discover_tools").await.is_some());
    assert!(mgr.get_tool("load_skill").await.is_some());
    assert!(mgr.get_tool("load_mcp").await.is_some());
}

#[tokio::test]
async fn stable_core_tools_are_registered_without_optional_providers() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();

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
    for name in ["files.read", "files.get_outline", "files.search"] {
        assert!(
            mgr.registry().get(name).await.is_none(),
            "convenience operation {name} must be loaded on demand"
        );
    }
    for name in ["shell", "http", "system.get_info"] {
        assert!(
            mgr.registry().get(name).await.is_some(),
            "direct builtin {name} must be provider-visible"
        );
    }
}

#[tokio::test]
async fn session_notes_and_history_operations_are_discoverable_and_loaded_on_demand() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let session_id = "ses-0123456789abcdef0123456789abcdef";
    let operation_names = [
        "session.read_notes",
        "session.replace_notes",
        "session.search_history",
        "session.read_history",
        "session.search_linked_history",
        "session.read_linked_history",
        "session.search_assets",
    ];
    let provider_tools = mgr.list_tool_definitions_for_session(session_id).await;

    for name in operation_names {
        assert!(mgr.registry().get(name).await.is_none());
        assert!(
            !provider_tools.iter().any(|tool| tool.name == name),
            "deferred session operation {name} must not be provider-visible before loading"
        );
    }

    let listed = mgr
        .execute_tool(
            Some(session_id),
            "discover_tools",
            json!({
                "action": "list",
                "source": "builtin",
                "level": "operations",
                "root": "session",
                "limit": 32
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let items = listed.output["items"].as_array().unwrap();
    for name in operation_names {
        let item = items
            .iter()
            .find(|item| item["name"] == name)
            .unwrap_or_else(|| panic!("tool catalog should expose {name}"));
        assert_eq!(item["loaded"], false);
    }
    assert!(!items.iter().any(|item| {
        item["name"] == "session.working_document_read"
            || item["name"] == "session.working_document_replace"
    }));

    let loaded = mgr
        .execute_tool(
            Some(session_id),
            "discover_tools",
            json!({
                "action": "load",
                "source": "builtin",
                "operations": operation_names
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(loaded.output["status"], "loaded");

    let provider_tools = mgr.list_tool_definitions_for_session(session_id).await;
    for name in operation_names {
        assert!(
            provider_tools.iter().any(|tool| tool.name == name),
            "loaded session operation {name} must enter the Agent's provider tool list"
        );
    }
}

#[tokio::test]
async fn test_discover_tools_exposes_three_layers_without_loading_deferred_tools() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let session_id = "ses-0123456789abcdef0123456789abcdef";

    let families = mgr
        .execute_tool(
            Some(session_id),
            "discover_tools",
            json!({"action": "list"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let family_items = families.output["items"].as_array().unwrap();
    for family in ["workflow", "interaction", "system", "extensions"] {
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
            "discover_tools",
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
        mgr.list_tool_definitions_for_session(session_id)
            .await
            .iter()
            .all(|def| def.name != "window.capture_screenshot")
    );

    let window_detail = mgr
        .execute_tool(
            Some(session_id),
            "discover_tools",
            json!({"action": "describe", "name": "window"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let operations = window_detail.output["operations"].as_array().unwrap();
    assert!(
        operations
            .iter()
            .any(|item| item["name"] == "window.capture_screenshot")
    );
    assert!(
        operations
            .iter()
            .all(|item| item.get("input_schema").is_none())
    );

    let operation_page = mgr
        .execute_tool(
            Some(session_id),
            "discover_tools",
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
            "discover_tools",
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
            "discover_tools",
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
            "discover_tools",
            json!({"action": "describe", "name": "window.capture_screenshot"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(screenshot_detail.output["loaded"], false);
    assert!(screenshot_detail.output["input_schema"].is_object());
    assert_eq!(screenshot_detail.output["load"]["tool"], "discover_tools");

    let loaded = mgr
        .execute_tool(
            Some(session_id),
            "discover_tools",
            {
                let operations = std::iter::once("window.capture_screenshot".to_string())
                    .chain((0..64).map(|index| format!("missing.operation_{index}")))
                    .collect::<Vec<_>>();
                json!({
                    "action": "load",
                    "source": "builtin",
                    "operations": operations
                })
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(loaded.success);
    assert_eq!(loaded.output["status"], "loaded");
    assert_eq!(
        loaded.output["missing_operations"]
            .as_array()
            .unwrap()
            .len(),
        64
    );
    assert!(
        mgr.get_tool_for_session(Some(session_id), "window.capture_screenshot")
            .await
            .is_some()
    );
}

#[tokio::test]
async fn discover_tools_describe_does_not_connect_or_execute_mcp() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
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
            "discover_tools",
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
    assert!(mgr.share_services().mcp.snapshot().await.is_empty());
}

#[tokio::test]
async fn test_tools_facade_disabled_tool_excluded_and_blocked() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();

    // Disable the `files` tool.
    let mut settings = HashMap::new();
    settings.insert(
        "files".into(),
        ToolConfig {
            enabled: false,
            ..Default::default()
        },
    );
    mgr.set_tool_settings(settings).await.unwrap();

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
async fn test_tools_facade_execute_builtin_tool() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let session_id = "ses-0123456789abcdef0123456789abcdef";
    assert!(
        mgr.load_builtin_operations_for_session(session_id, Some(vec!["files.read".into()]), None)
            .await
    );

    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("hello.txt");
    tokio::fs::write(&file, "hello from manager").await.unwrap();

    let result = mgr
        .execute_tool(
            Some(session_id),
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
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();

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
async fn test_tools_facade_resolve_risk_level_unknown() {
    let mgr = ToolsFacade::new();
    let risk = mgr
        .resolve_risk_level(None, "nonexistent", &json!({}))
        .await
        .unwrap();
    assert_eq!(risk, RiskLevel::Safe);
}

#[tokio::test]
async fn authorization_policy_rejects_schema_valid_noncanonical_routing_scope() {
    struct RoutedTool;

    #[async_trait::async_trait]
    impl Tool for RoutedTool {
        fn name(&self) -> String {
            "files".into()
        }

        fn description(&self) -> String {
            "routing policy validation probe".into()
        }

        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::High
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string"},
                    "operation": {"type": "string"}
                },
                "required": ["scope", "operation"],
                "additionalProperties": false
            })
        }

        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({"executed": true})))
        }
    }

    let mgr = ToolsFacade::new();
    mgr.register_for_session("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", Arc::new(RoutedTool))
        .await;
    let input = json!({"scope": "bad scope", "operation": "read"});
    let tool = mgr
        .get_tool_for_session(Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "files")
        .await
        .unwrap();
    assert!(tool.validate_input(&input).is_ok());

    let error = mgr
        .resolve_authorization_request(
            Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "files",
            &input,
        )
        .await
        .unwrap_err();
    assert!(error.contains("invalid capability scope"));
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

    let mgr = ToolsFacade::new();
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
async fn execute_tool_retries_transient_failure_within_shared_limit() {
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
            if self.attempts.fetch_add(1, Ordering::SeqCst) < haven_common::config::MAX_TOOL_RETRIES
            {
                anyhow::bail!("service unavailable")
            }
            Ok(ToolResult::ok(json!({"recovered": true})))
        }
    }

    let mgr = ToolsFacade::new();
    let attempts = Arc::new(AtomicU32::new(0));
    mgr.set_tool_settings(HashMap::from([(
        "flaky".into(),
        ToolConfig {
            max_retries: Some(haven_common::config::MAX_TOOL_RETRIES),
            retry_backoff_secs: Some(0),
            ..Default::default()
        },
    )]))
    .await
    .unwrap();
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
    assert_eq!(attempts.load(Ordering::SeqCst), 11);
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
    let manager = ToolsFacade::new();
    manager
        .set_tool_settings(HashMap::from([(
            "unknown_side_effect".into(),
            ToolConfig {
                max_retries: Some(3),
                retry_backoff_secs: Some(0),
                ..Default::default()
            },
        )]))
        .await
        .unwrap();
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

    let mgr = ToolsFacade::new();
    mgr.set_tool_settings(HashMap::from([(
        "slow_intrinsic".into(),
        ToolConfig {
            max_output_chars: Some(100),
            ..Default::default()
        },
    )]))
    .await
    .unwrap();
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

    let mgr = ToolsFacade::new();
    let attempts = Arc::new(AtomicU32::new(0));
    mgr.set_tool_settings(HashMap::from([(
        "intrinsic_retry".into(),
        ToolConfig {
            max_output_chars: Some(100),
            ..Default::default()
        },
    )]))
    .await
    .unwrap();
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
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();

    // Before registering a per-session tool, schemas come only from the
    // global registry.
    let base_schemas = mgr
        .list_schemas_for_session("ses-8042867cb780e3dfc14959c1fe8d87e1")
        .await;
    let base_count = base_schemas.len();

    // Register a fake per-session tool.
    let root = tempfile::tempdir().unwrap();
    let skill =
        crate::test_support::discover_skill_fixture(root.path(), "demo", "demo skill", None).await;
    let runner = mgr.coordinator.builtins.skill_runner.read().await.clone();
    let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
    mgr.register_for_session("ses-8042867cb780e3dfc14959c1fe8d87e1", Arc::new(adapter))
        .await;

    let schemas = mgr
        .list_schemas_for_session("ses-8042867cb780e3dfc14959c1fe8d87e1")
        .await;
    assert_eq!(
        schemas.len(),
        base_count + 1,
        "per-session skill tool should appear in schemas"
    );
    assert!(schemas.iter().any(|s| s["name"] == "skill__demo"));

    // Other sessions should NOT see this tool.
    let other = mgr
        .list_schemas_for_session("ses-f54924ed1bcde71cb1d3e0e9d437e3e4")
        .await;
    assert_eq!(other.len(), base_count);
    assert!(!other.iter().any(|s| s["name"] == "skill__demo"));
}

#[tokio::test]
async fn test_session_tool_overlay_version_does_not_invalidate_other_sessions() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let before_a = mgr
        .catalog_version_for_session("ses-8042867cb780e3dfc14959c1fe8d87e1")
        .await;
    let before_b = mgr
        .catalog_version_for_session("ses-f54924ed1bcde71cb1d3e0e9d437e3e4")
        .await;

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

    mgr.register_for_session(
        "ses-8042867cb780e3dfc14959c1fe8d87e1",
        Arc::new(NamedStub("session_only")),
    )
    .await;
    let after_a = mgr
        .catalog_version_for_session("ses-8042867cb780e3dfc14959c1fe8d87e1")
        .await;
    let after_b = mgr
        .catalog_version_for_session("ses-f54924ed1bcde71cb1d3e0e9d437e3e4")
        .await;
    assert_eq!(
        after_a.global_catalog_version,
        before_a.global_catalog_version
    );
    assert_eq!(
        after_a.session_overlay_version,
        before_a.session_overlay_version + 1
    );
    assert_eq!(after_b, before_b);

    mgr.rebuild_catalog().await.unwrap();
    let after_global_a = mgr
        .catalog_version_for_session("ses-8042867cb780e3dfc14959c1fe8d87e1")
        .await;
    let after_global_b = mgr
        .catalog_version_for_session("ses-f54924ed1bcde71cb1d3e0e9d437e3e4")
        .await;
    assert!(after_global_a.global_catalog_version > after_a.global_catalog_version);
    assert_eq!(
        after_global_a.session_overlay_version,
        after_a.session_overlay_version
    );
    assert_eq!(
        after_global_b.session_overlay_version,
        after_b.session_overlay_version
    );
}

#[tokio::test]
async fn live_output_tools_receive_session_without_step_id() {
    use std::sync::Mutex;

    struct CaptureInput(Arc<Mutex<Option<Value>>>);
    #[async_trait::async_trait]
    impl Tool for CaptureInput {
        fn name(&self) -> String {
            "capture_live_input".into()
        }
        fn description(&self) -> String {
            "capture trusted execution metadata".into()
        }
        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn supports_live_output(&self) -> bool {
            true
        }
        async fn execute(&self, input: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            *self.0.lock().unwrap() = Some(input);
            Ok(ToolResult::ok(json!({})))
        }
    }

    let mgr = ToolsFacade::new();
    let captured = Arc::new(Mutex::new(None));
    mgr.registry()
        .register(Arc::new(CaptureInput(captured.clone())))
        .await
        .unwrap();

    mgr.execute_tool_with_step(
        Some("ses-0f0ffb58ed44958c450b5c9d0bbb91bc"),
        "capture_live_input",
        json!({"_session_id": "ses-forged", "_step_id": "step-forged"}),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    let captured = captured.lock().unwrap().clone().unwrap();
    assert_eq!(
        captured["_session_id"],
        "ses-0f0ffb58ed44958c450b5c9d0bbb91bc"
    );
    assert!(captured.get("_step_id").is_none());
}

#[tokio::test]
async fn test_build_mcp_index_filters_disabled() {
    use haven_common::config::McpServerConfig;

    let mgr = ToolsFacade::new();
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
    let names: Vec<&str> = index.iter().map(|entry| entry.name.as_str()).collect();
    assert!(names.contains(&"on"));
    assert!(!names.contains(&"off"), "disabled server should not appear");
}

#[test]
fn mcp_server_index_entry_sanitizes_sorts_and_deduplicates() {
    let entry = McpServerIndexEntry::from_raw(
        "research\nserver",
        vec!["z_search".into(), "a_fetch".into(), "z_search".into()],
    );

    assert_eq!(entry.name, "research server");
    assert_eq!(entry.tool_names, ["a_fetch", "z_search"]);
}

#[test]
fn mcp_search_detection_only_uses_cached_tool_names() {
    assert!(
        crate::runtime_capabilities::mcp_index_entry_has_search_tool(&McpServerIndexEntry {
            name: "research".into(),
            tool_names: vec!["fetch".into(), "web_search".into()],
        })
    );
    assert!(
        !crate::runtime_capabilities::mcp_index_entry_has_search_tool(&McpServerIndexEntry {
            name: "search-like-server".into(),
            tool_names: vec![],
        })
    );
    assert!(
        !crate::runtime_capabilities::mcp_index_entry_has_search_tool(&McpServerIndexEntry {
            name: "research".into(),
            tool_names: vec!["fetch".into()],
        })
    );
}

#[tokio::test]
async fn test_build_mcp_index_does_not_expose_process_args() {
    use haven_common::config::McpServerConfig;

    let mgr = ToolsFacade::new();
    mgr.upsert_mcp_server_config(McpServerConfig {
        name: "safe-server".into(),
        command: "server.exe".into(),
        args: vec!["--token".into(), "SECRET_SHOULD_NOT_REACH_PROMPT".into()],
        enabled: true,
        ..Default::default()
    })
    .await;

    let index = mgr.build_mcp_index().await;
    let rendered = format!("{:?}", index[0]);
    assert!(!rendered.contains("server.exe"));
    assert!(!rendered.contains("SECRET_SHOULD_NOT_REACH_PROMPT"));
    assert!(index[0].name.contains("safe-server"));
}

#[tokio::test]
async fn test_upsert_and_remove_mcp_server_config() {
    use haven_common::config::McpServerConfig;

    let mgr = ToolsFacade::new();
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
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let schemas = mgr.registry().list_schemas().await;
    assert!(
        !schemas
            .iter()
            .any(|s| { s["name"].as_str().unwrap_or("").starts_with("mcp__") }),
        "MCP tools must not be pre-registered globally"
    );
}

#[tokio::test]
async fn direct_tools_stay_on_provider_surface_while_deferred_builtins_load_on_demand() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();

    assert!(mgr.registry().get("shell").await.is_some());
    assert!(mgr.registry().get("http").await.is_some());
    assert!(mgr.registry().get("system.get_info").await.is_some());
    assert!(
        mgr.list_tool_definitions_for_session("ses-334aafdac2a2c6de43c6daf3805f41d5")
            .await
            .iter()
            .any(|def| def.name == "shell")
    );

    let result = mgr
        .execute_tool(
            Some("ses-334aafdac2a2c6de43c6daf3805f41d5"),
            "discover_tools",
            serde_json::json!({
                "action": "load",
                "source": "builtin",
                "operations": ["files.read"]
            }),
            CancellationToken::new(),
        )
        .await
        .expect("discover_tools load should be executable from the core surface");
    assert!(result.success, "loader failed: {:?}", result.error);
    assert_eq!(result.output["status"], "loaded");
    assert!(result.output.get("input_schema").is_none());

    let loaded = mgr
        .list_tool_definitions_for_session("ses-334aafdac2a2c6de43c6daf3805f41d5")
        .await;
    assert!(loaded.iter().any(|def| def.name == "files.read"));
    assert!(loaded.iter().any(|def| def.name == "system.get_info"));
}

#[tokio::test]
async fn test_list_tool_definitions_for_session_caps_at_max_tools() {
    use haven_common::config::ContextLimitsConfig;

    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let global = mgr.registry().list_tool_definitions().await.len();
    assert!(global > 0, "catalog should have builtins");

    // Leave room for only 2 session overlays. Write the limit directly so
    // we do not rebuild the catalog (and shift `global`) mid-test.
    let limits = ContextLimitsConfig {
        max_tools_per_request: global + 2,
        ..Default::default()
    };
    mgr.coordinator
        .runtime
        .update_platform(|current| {
            let mut next = current.clone();
            next.context_limits = limits;
            next
        })
        .await;

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
        mgr.register_for_session(
            "ses-459099354ce5fa7f25d31bab931dba1e",
            Arc::new(NamedStub(name)),
        )
        .await;
    }

    let defs = mgr
        .list_tool_definitions_for_session("ses-459099354ce5fa7f25d31bab931dba1e")
        .await;
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

    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let hits2 = hits.clone();
    mgr.coordinator
        .runtime
        .live_outputs
        .set_live_output_event_sink(Arc::new(move |_event, payload| {
            if let Some(sid) = payload["step_id"].as_str() {
                hits2.lock().unwrap().push(sid.to_string());
            }
        }));

    // Forged private fields, no trusted step → no live emit.
    let _ = mgr
        .execute_tool(
            Some("ses-09df36791e54c51f7be063867e10b9af"),
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
            Some("ses-09df36791e54c51f7be063867e10b9af"),
            "shell",
            json!({
                "command": "echo trusted",
                "shell": "cmd",
                "_step_id": "step-forged",
            }),
            CancellationToken::new(),
            Some("step-00000000000000000000000000000009"),
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
        seen.iter()
            .all(|s| s == "step-00000000000000000000000000000009"),
        "only trusted step id may be emitted, got {seen:?}"
    );
}
#[tokio::test]
async fn registered_operation_manifest_matches_runtime_policy() {
    let mgr = ToolsFacade::new();
    mgr.rebuild_catalog().await.unwrap();
    let mut tools = mgr.registry().list().await;
    tools.extend(mgr.operations().deferred().list().await);
    let find = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .clone()
    };

    for name in [
        "ask",
        "notify",
        "shell",
        "http",
        "system.get_info",
        "discover_tools",
        "load_skill",
        "load_mcp",
        "files.read",
        "files.list",
        "agent.list_inbox",
    ] {
        assert!(find(name).operation_spec().is_some(), "{name} spec");
    }

    for (name, group) in [
        ("ask", haven_common::tools::ToolCatalogGroup::Interaction),
        ("notify", haven_common::tools::ToolCatalogGroup::Interaction),
        ("shell", haven_common::tools::ToolCatalogGroup::Shell),
        ("http", haven_common::tools::ToolCatalogGroup::Network),
        (
            "system.get_info",
            haven_common::tools::ToolCatalogGroup::System,
        ),
        (
            "discover_tools",
            haven_common::tools::ToolCatalogGroup::Extensions,
        ),
        (
            "load_skill",
            haven_common::tools::ToolCatalogGroup::Extensions,
        ),
        (
            "load_mcp",
            haven_common::tools::ToolCatalogGroup::Extensions,
        ),
        ("files.read", haven_common::tools::ToolCatalogGroup::Files),
        (
            "agent.list_inbox",
            haven_common::tools::ToolCatalogGroup::Workflow,
        ),
    ] {
        assert_eq!(find(name).catalog_group(), group, "{name}");
    }

    for tool in &tools {
        let name = tool.name();
        // Git requires a subcommand. An empty object intentionally projects a
        // restrictive invalid-input fallback, so use its ordinary local
        // read-only operation when comparing the static manifest to runtime
        // policy.
        let policy_input = if name == "git" {
            json!({"subcommand": "status"})
        } else {
            json!({})
        };
        let runtime = tool.operation_policy(&policy_input);
        let manifest = tool.tool_manifest();
        let def = tool.tool_def();
        assert_eq!(tool.catalog_group(), def.catalog_group, "{name}");
        assert_eq!(manifest.policy.effect, runtime.effect, "{name}");
        assert_eq!(manifest.policy.idempotency, runtime.idempotency, "{name}");
        assert_eq!(manifest.policy.scope, runtime.scope, "{name}");
        let concurrency_mode = match runtime.concurrency {
            ToolConcurrency::ReadOnly => ToolConcurrencyMode::ReadOnly,
            ToolConcurrency::SharedResource(_) => ToolConcurrencyMode::SharedResource,
            ToolConcurrency::Resource(_) => ToolConcurrencyMode::Resource,
            ToolConcurrency::Exclusive => ToolConcurrencyMode::Exclusive,
        };
        assert_eq!(manifest.policy.concurrency, concurrency_mode, "{name}");
        if name == "files.search" {
            assert_eq!(runtime.risk_level, RiskLevel::Low, "{name}");
            assert_eq!(
                runtime.confirmation,
                ConfirmationRequirement::None,
                "{name}"
            );
            assert_eq!(manifest.policy.risk_level, RiskLevel::Medium, "{name}");
            assert_eq!(
                manifest.policy.confirmation,
                ConfirmationRequirement::SecurityPolicy,
                "{name}"
            );
            continue;
        }
        assert_eq!(manifest.policy.risk_level, runtime.risk_level, "{name}");
        assert_eq!(manifest.policy.confirmation, runtime.confirmation, "{name}");
    }

    let files_read = find("files.read").operation_policy(&json!({}));
    assert_eq!(files_read.effect, OperationEffect::ReadOnly);
    assert_eq!(files_read.confirmation, ConfirmationRequirement::None);
    assert_eq!(files_read.risk_level, RiskLevel::Low);

    let system_info = find("system.get_info").operation_policy(&json!({}));
    assert_eq!(system_info.scope, ToolOperationScope::Global);
    assert_eq!(system_info.effect, OperationEffect::ReadOnly);

    let search = find("files.search");
    let content = search.operation_policy(&json!({"mode": "content"}));
    assert_eq!(content.risk_level, RiskLevel::Medium);
    assert_eq!(content.confirmation, ConfirmationRequirement::None);

    let http = find("http");
    let get = http.operation_policy(&json!({}));
    assert_eq!(get.idempotency, OperationIdempotency::Idempotent);
    assert_eq!(
        get.concurrency,
        ToolConcurrency::SharedResource("http".into())
    );
    assert_eq!(get.confirmation, ConfirmationRequirement::SecurityPolicy);
    let post = http.operation_policy(&json!({"method": "POST"}));
    assert_eq!(post.idempotency, OperationIdempotency::NonIdempotent);
    assert_eq!(post.concurrency, ToolConcurrency::Resource("http".into()));
    assert_eq!(post.confirmation, get.confirmation);
    let other = http.operation_policy(&json!({"method": "PUT"}));
    assert_eq!(other.idempotency, OperationIdempotency::Unknown);
    assert_eq!(other.concurrency, ToolConcurrency::Resource("http".into()));

    for (name, represented, root, operation) in [
        ("load_skill", ToolSource::Skill, "skills", "load"),
        ("load_mcp", ToolSource::Mcp, "mcp", "load"),
    ] {
        let manifest = find(name).tool_manifest();
        assert_eq!(manifest.identity.source, ToolSource::Builtin, "{name}");
        assert_eq!(
            manifest.presentation.represented_source, represented,
            "{name}"
        );
        assert_eq!(manifest.identity.root, root, "{name}");
        assert_eq!(
            manifest.identity.operation.as_deref(),
            Some(operation),
            "{name}"
        );
    }

    let ask = find("ask");
    assert_eq!(ask.idempotency(&json!({})), OperationIdempotency::Unknown);
    assert_eq!(
        ask.idempotency(&json!({"question": "continue?"})),
        OperationIdempotency::NonIdempotent
    );
    assert_eq!(
        ask.tool_def().retry_safety,
        OperationIdempotency::NonIdempotent.tool_retry_safety()
    );

    assert_eq!(
        find("agent.list_inbox")
            .operation_policy(&json!({}))
            .idempotency,
        OperationIdempotency::Idempotent
    );

    // Provider-backed media operations are absent until a live capability is
    // wired. Their frozen concurrency is covered with every capability enabled.
    for name in [
        "media.describe",
        "media.run_ocr",
        "media.transcribe",
        "media.generate",
        "media.record",
        "media.speak",
    ] {
        assert!(
            tools.iter().all(|tool| tool.name() != name),
            "{name} must stay unregistered until its capability exists"
        );
    }
    for name in ["media.inspect", "media.extract", "media.render"] {
        assert_eq!(
            find(name).operation_policy(&json!({})).concurrency,
            ToolConcurrency::Resource("media:unknown".into()),
            "{name}"
        );
    }
    for name in [
        "media.play",
        "media.get_volume",
        "media.set_volume",
        "media.get_mute",
        "media.set_mute",
    ] {
        assert_eq!(
            find(name).operation_policy(&json!({})).concurrency,
            ToolConcurrency::Resource("media:audio-device".into()),
            "{name}"
        );
    }
}
