//! Tool execution, safety-gated confirms, and ToolCall-step persistence.
//!
//! Split from `session.rs` (Phase 7 / A3 mechanical extract). R2: confirm never
//! blocks inside a tool future — ReAct uses pause/continue; scheduled fires
//! use [`SessionSupervisor::request_scheduled_confirm`].

use super::*;
use haven_memory::ToolStepOutcome;

mod confirmation;
mod execution;
mod step_persistence;

pub(super) use confirmation::confirmation_expiry_delay;
#[cfg(test)]
use confirmation::{
    ScheduledConfirmDeadlineDecision, interaction_owner_matches_request,
    scheduled_confirmation_is_expired,
};
use step_persistence::{ToolStepContext, ToolStepRequest, tool_step_outcome};
pub(crate) use step_persistence::{ToolStepMetadata, ToolStepPersistenceError};
#[cfg(test)]
mod scheduled_authorization_tests {
    use super::*;
    use haven_common::types::{NetworkPolicy, PermissionMode, SandboxMode};
    use haven_memory::Database;
    use haven_tools::{AuthorizationDecision, AuthorizationReasonCode};
    use serde_json::json;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct PolicyTestTool {
        name: String,
        risk_level: RiskLevel,
        on_execute: Option<Arc<dyn Fn() + Send + Sync>>,
        result: Option<ToolResult>,
    }

    struct CodeModeInvocationTestTool {
        definitions: Vec<haven_tools::CodeModeToolDefinition>,
    }

    struct WorkflowInvocationTestTool {
        definitions: Vec<haven_tools::WorkflowCapabilityDefinition>,
    }

    struct ChildStepCaptureTool {
        name: String,
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl haven_tools::Tool for ChildStepCaptureTool {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn description(&self) -> String {
            "test-only child step identity capture".into()
        }

        fn risk_level(&self, _input: &Value) -> RiskLevel {
            RiskLevel::Safe
        }

        fn supports_live_output(&self) -> bool {
            true
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object", "additionalProperties": true})
        }

        async fn execute(
            &self,
            input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            let step_id = input
                .get("_step_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("child call did not receive a step identity"))?;
            self.captured
                .lock()
                .expect("child identity capture mutex poisoned")
                .push(step_id.to_owned());
            Ok(ToolResult::from_output(json!({"ok": true}), false))
        }
    }

    #[async_trait::async_trait]
    impl haven_tools::Tool for CodeModeInvocationTestTool {
        fn name(&self) -> String {
            haven_tools::CODEMODE_RUN_TOOL_NAME.to_owned()
        }

        fn description(&self) -> String {
            "test-only bounded CodeMode invocation".into()
        }

        fn risk_level(&self, _input: &Value) -> RiskLevel {
            RiskLevel::Safe
        }

        fn codemode_tool_definitions(&self) -> Option<Vec<haven_tools::CodeModeToolDefinition>> {
            Some(self.definitions.clone())
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {"script": {"type": "string"}},
                "required": ["script"],
                "additionalProperties": false
            })
        }

        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            anyhow::bail!("CodeMode test tool requires its subcall port")
        }

        async fn execute_with_codemode_subcalls(
            &self,
            input: Value,
            cancel: CancellationToken,
            target_binding: Option<&str>,
            subcalls: Arc<dyn haven_tools::CodeModeSubcallPort>,
        ) -> anyhow::Result<ToolResult> {
            if target_binding.is_some() {
                anyhow::bail!("CodeMode test invocation cannot have an external target")
            }
            let script = input
                .get("script")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("CodeMode test input requires a script"))?;
            let execution = haven_tools::CodeModeEvaluator::default()
                .execute(script, self.definitions.clone(), subcalls, cancel)
                .await
                .map_err(anyhow::Error::msg)?;
            Ok(ToolResult::from_output(
                json!({"output": execution.output, "call_count": execution.call_count}),
                false,
            ))
        }
    }

    #[async_trait::async_trait]
    impl haven_tools::Tool for WorkflowInvocationTestTool {
        fn name(&self) -> String {
            haven_tools::WORKFLOW_RUN_TOOL_NAME.to_owned()
        }

        fn description(&self) -> String {
            "test-only declarative Workflow invocation".into()
        }

        fn risk_level(&self, _input: &Value) -> RiskLevel {
            RiskLevel::Safe
        }

        fn workflow_capability_definitions(
            &self,
        ) -> Option<Vec<haven_tools::WorkflowCapabilityDefinition>> {
            Some(self.definitions.clone())
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }

        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            anyhow::bail!("Workflow test tool requires its subcall port")
        }

        async fn execute_with_workflow_subcalls(
            &self,
            input: Value,
            cancel: CancellationToken,
            target_binding: Option<&str>,
            subcalls: Arc<dyn haven_tools::WorkflowSubcallPort>,
        ) -> anyhow::Result<ToolResult> {
            if target_binding.is_some() {
                anyhow::bail!("Workflow test invocation cannot have an external target")
            }
            let step = input
                .pointer("/workflow/steps/0")
                .ok_or_else(|| anyhow::anyhow!("missing workflow step"))?;
            let step_id = step
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing workflow step id"))?;
            let capability = step
                .get("capability")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing workflow capability"))?;
            if !self
                .definitions
                .iter()
                .any(|definition| definition.name == capability)
            {
                anyhow::bail!("workflow capability is outside the test allowlist");
            }
            let input_fields = step
                .get("input")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("missing workflow input"))?;
            let child_input = input_fields
                .iter()
                .map(|(name, mapping)| {
                    if mapping.get("kind").and_then(Value::as_str) != Some("literal") {
                        anyhow::bail!("test workflow accepts only literal input mappings");
                    }
                    let value = mapping
                        .get("value")
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("missing literal mapping value"))?;
                    Ok((name.clone(), value))
                })
                .collect::<anyhow::Result<serde_json::Map<_, _>>>()?;
            let child = subcalls
                .call(&step_id, capability, Value::Object(child_input), cancel)
                .await
                .map_err(anyhow::Error::msg)?;
            Ok(ToolResult::from_output(
                json!({
                    "status": "completed",
                    "completed_step_ids": [step_id],
                    "steps": [{
                        "step_id": step_id,
                        "capability": capability,
                        "observation": child.observation
                    }]
                }),
                false,
            ))
        }
    }

    #[async_trait::async_trait]
    impl haven_tools::Tool for PolicyTestTool {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn description(&self) -> String {
            "test-only authorization policy".into()
        }

        fn risk_level(&self, _input: &Value) -> RiskLevel {
            self.risk_level
        }

        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<haven_tools::ToolResult> {
            if let Some(on_execute) = &self.on_execute {
                on_execute();
                return Ok(self
                    .result
                    .clone()
                    .unwrap_or_else(|| haven_tools::ToolResult::ok(json!({"ok": true}))));
            }
            if let Some(result) = &self.result {
                return Ok(result.clone());
            }
            unreachable!("scheduled authorization tests never execute a tool")
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
    }

    fn test_supervisor() -> (
        Arc<SessionSupervisor>,
        Arc<haven_tools::ToolsFacade>,
        Arc<Database>,
        tempfile::TempDir,
    ) {
        let tools = Arc::new(haven_tools::ToolsFacade::new());
        let directory = tempfile::tempdir().unwrap();
        let database =
            Arc::new(Database::open(&directory.path().join("authorization.db")).unwrap());
        let supervisor = Arc::new(SessionSupervisor::new_for_test(
            database.clone(),
            tools.clone(),
            1,
        ));
        (supervisor, tools, database, directory)
    }

    async fn allow_opaque_capabilities_for_test(supervisor: &SessionSupervisor) {
        supervisor
            .authorization
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Ask)
            .await;
    }

    #[tokio::test]
    async fn invalid_capability_policy_fails_before_tool_execution() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: "files".into(),
                    risk_level: RiskLevel::High,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;

        let result = supervisor
            .execute_gated(
                Some(session_id),
                "files",
                json!({"scope": "bad scope", "operation": "read"}),
                CancellationToken::new(),
                None,
                None,
            )
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("invalid capability policy unexpectedly reached execution"),
        };

        assert!(error.to_string().contains("invalid capability scope"));
        assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    fn future_receipt(
        confirmation_id: haven_common::types::ConfirmId,
        capability: &str,
    ) -> haven_tools::ConfirmationReceipt {
        haven_tools::ConfirmationReceipt {
            confirmation_id,
            capability: haven_common::types::CapabilityScope::try_new(capability).unwrap(),
            canonical_input_hash: String::new(),
            effective_risk: RiskLevel::High,
            policy_revision: 1,
            expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
        }
    }

    #[test]
    fn accepted_scheduled_decision_keeps_its_deadline_verdict_after_persistence() {
        let mut request = crate::interaction::InteractionRequest::scheduled_confirm(
            "toolrun-1234567890abcdef1234567890abcdef".into(),
            None,
            "files.write".into(),
            json!({}),
            future_receipt(haven_common::types::ConfirmId::new(), "files.write"),
            "scheduled".into(),
        );
        request.expires_at = Some((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());

        assert!(scheduled_confirmation_is_expired(
            &request,
            ScheduledConfirmDeadlineDecision::CheckAtResolution
        ));
        assert!(!scheduled_confirmation_is_expired(
            &request,
            ScheduledConfirmDeadlineDecision::AcceptedBeforeDeadline
        ));
        assert!(scheduled_confirmation_is_expired(
            &request,
            ScheduledConfirmDeadlineDecision::Expire
        ));
    }

    async fn running_scheduled_tool_run(
        supervisor: &SessionSupervisor,
        session_id: Option<&str>,
        tool_name: &str,
    ) -> String {
        let _receiver = supervisor
            .tool_run_port()
            .take_completion_receiver()
            .expect("scheduled receiver");
        let tool_runs = supervisor.tool_run_test_support();
        let tool_run_id = tool_runs
            .schedule(haven_tools::ScheduledToolRunSpec {
                due_at: Some((chrono::Utc::now() + chrono::Duration::seconds(3)).to_rfc3339()),
                delay_secs: None,
                watch_tool_run_id: None,
                title: "confirmation arbitration".into(),
                body: "test scheduled execution owner".into(),
                mode: haven_tools::ScheduleMode::Tool,
                session_id: session_id.map(str::to_owned),
                tool_name: Some(tool_name.to_string()),
                tool_args: Some(json!({})),
                prompt: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(6), async {
            loop {
                if tool_runs.status_view(&tool_run_id).await.status()
                    == Some(haven_common::lifecycle::ToolRunStatus::Running)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("scheduled ToolRun should enter Running before confirmation");
        tool_run_id
    }

    #[tokio::test]
    async fn scheduled_confirmation_persists_failed_tool_result_as_failed() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("scheduled failed result status")
            .await
            .unwrap();
        let tool_name = "scheduled.failed-result";
        let input = json!({"operation": "test"});
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                &session.session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: Some(Arc::new(|| {})),
                    result: Some(ToolResult::failed(
                        json!({"error": "expected failure"}),
                        "expected tool failure",
                    )),
                }),
            )
            .await;

        let authorization_request = supervisor
            .scheduled_authorization_request(Some(&session.session_id), tool_name, &input)
            .await
            .unwrap();
        let receipt = match supervisor
            .authorization
            .authorize(&authorization_request)
            .await
        {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("expected confirmation, got {decision:?}"),
        };
        let tool_run_id =
            running_scheduled_tool_run(&supervisor, Some(&session.session_id), tool_name).await;
        let request = crate::interaction::InteractionRequest::scheduled_confirm(
            tool_run_id.clone(),
            Some(&session.session_id),
            tool_name.into(),
            input,
            receipt,
            "failed result test".into(),
        );
        let mut events = supervisor.subscribe_events();

        supervisor
            .finish_scheduled_confirm(request.clone(), true, false)
            .await;

        assert_eq!(
            supervisor
                .tool_run_test_support()
                .status_view(&tool_run_id)
                .await
                .status(),
            Some(haven_common::lifecycle::ToolRunStatus::Failed)
        );
        assert!(matches!(
            events.try_recv(),
            Ok(SessionSupervisorEvent::ScheduledConfirmOutcome { .. })
        ));

        // A competing terminal transition that already won must not produce
        // a second user-visible completion event.
        supervisor
            .finish_scheduled_confirm(request, false, false)
            .await;
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn scheduled_authorization_preserves_request_and_decision_behavior() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-00000000000000000000000000000001";
        let tool_name = "scheduled.critical";
        let input = json!({"target": "recording"});
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::Critical,
                    on_execute: None,
                    result: None,
                }),
            )
            .await;

        let request = supervisor
            .scheduled_authorization_request(Some(session_id), tool_name, &input)
            .await
            .unwrap();
        assert_eq!(request.session_id.as_deref(), Some(session_id));
        assert_eq!(request.tool_name, tool_name);
        assert_eq!(request.input, input);
        assert_eq!(request.policy.risk_level, RiskLevel::Critical);
        assert_eq!(
            request.policy.capability.to_string(),
            haven_common::types::permission_key(tool_name, &input)
        );
        assert_eq!(
            request.policy.confirmation,
            haven_common::tools::ConfirmationRequirement::Required
        );

        assert!(matches!(
            supervisor
                .authorize_scheduled_tool(Some(session_id), tool_name, &input)
                .await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));

        let safe_tool_name = "scheduled.safe";
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: safe_tool_name.into(),
                    risk_level: RiskLevel::Safe,
                    on_execute: None,
                    result: None,
                }),
            )
            .await;
        assert!(matches!(
            supervisor
                .authorize_scheduled_tool(Some(session_id), safe_tool_name, &json!({}))
                .await,
            AuthorizationDecision::AutoApproved
        ));

        supervisor
            .authorization
            .set_permission_mode(PermissionMode::Plan)
            .await;
        assert!(matches!(
            supervisor
                .authorize_scheduled_tool(Some(session_id), tool_name, &input)
                .await,
            AuthorizationDecision::Blocked {
                reason_code: AuthorizationReasonCode::PlanMode,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn codemode_subcalls_execute_with_a_fresh_auto_approved_proof() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-12121212121212121212121212121212";
        let tool_name = "scheduled.safe";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::Safe,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;

        let port = supervisor.codemode_subcall_port(session_id, [tool_name.to_owned()]);
        let result = port
            .call(
                0,
                tool_name,
                json!({"operation": "read"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, json!({"ok": true}));
        assert!(executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn codemode_child_calls_receive_unique_canonical_step_ids() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-41414141414141414141414141414141";
        let tool_name = "scheduled.step_capture";
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(ChildStepCaptureTool {
                    name: tool_name.into(),
                    captured: captured.clone(),
                }),
            )
            .await;

        let port = supervisor.codemode_subcall_port(session_id, [tool_name.to_owned()]);
        for call_index in 0..2 {
            port.call(call_index, tool_name, json!({}), CancellationToken::new())
                .await
                .expect("CodeMode child call should execute");
        }

        let step_ids = captured.lock().unwrap().clone();
        assert_eq!(step_ids.len(), 2);
        assert!(
            step_ids
                .iter()
                .all(|step_id| haven_common::types::is_canonical_id(step_id, "step"))
        );
        assert_ne!(step_ids[0], step_ids[1]);
    }

    #[tokio::test]
    async fn gated_codemode_invocation_routes_child_through_agent_and_tools_authorization() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-78787878787878787878787878787878";
        let child_name = "scheduled.safe";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: child_name.into(),
                    risk_level: RiskLevel::Safe,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;
        let definitions = vec![haven_tools::CodeModeToolDefinition {
            name: child_name.into(),
            description: "read the scheduled test value".into(),
            input_schema: json!({"type": "object"}),
        }];
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(CodeModeInvocationTestTool { definitions }),
            )
            .await;

        let execution = supervisor
            .execute_gated(
                Some(session_id),
                haven_tools::CODEMODE_RUN_TOOL_NAME,
                json!({
                    "script": "const value = await tools.call('scheduled.safe', {operation: 'read'}); return value.output;"
                }),
                CancellationToken::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert!(execution.result.success, "{:?}", execution.result.error);
        assert_eq!(execution.result.output["output"]["ok"], true);
        assert_eq!(execution.result.output["call_count"], 1);
        assert!(executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn codemode_confirmation_required_child_fails_before_side_effect() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-34343434343434343434343434343434";
        let tool_name = "scheduled.critical";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;

        let port = supervisor.codemode_subcall_port(session_id, [tool_name.to_owned()]);
        let error = port
            .call(
                0,
                tool_name,
                json!({"operation": "write"}),
                CancellationToken::new(),
            )
            .await
            .expect_err("nested confirmation must not inherit the outer script approval");

        assert!(error.contains("requires its own user confirmation"));
        assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
        let nested = port
            .pending_confirmation()
            .expect("CodeMode must retain the exact child confirmation request");
        assert_eq!(nested.call_index, 0);
        let resumed = supervisor
            .codemode_subcall_port_with_resume(
                session_id,
                [tool_name.to_owned()],
                None,
                Some(nested),
            )
            .unwrap();
        let result = resumed
            .call(
                0,
                tool_name,
                json!({"operation": "write"}),
                CancellationToken::new(),
            )
            .await
            .expect("the resumed child should execute with its receipt");
        assert!(result.success);
        assert!(executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn codemode_subcall_port_rejects_names_outside_its_run_allowlist() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-56565656565656565656565656565656";
        let tool_name = "scheduled.safe";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::Safe,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;

        let port = supervisor.codemode_subcall_port(session_id, std::iter::empty());
        let error = port
            .call(0, tool_name, json!({}), CancellationToken::new())
            .await
            .expect_err("unlisted tool must not reach authorization or execution");

        assert!(error.contains("allowlist"));
        assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn workflow_subcalls_execute_with_a_fresh_auto_approved_proof() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        allow_opaque_capabilities_for_test(&supervisor).await;
        let session_id = "ses-76767676767676767676767676767676";
        let tool_name = "mcp__notes__read";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::Safe,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;

        let request = supervisor
            .authorization
            .authorization_request(Some(session_id), tool_name, &json!({"id": "n-1"}))
            .await
            .unwrap();
        supervisor
            .authorization
            .grant(
                Some(session_id),
                request.policy.capability,
                haven_common::types::PermissionEffect::Allow,
                haven_common::types::PermissionScope::Session,
            )
            .await;

        let port = supervisor.workflow_subcall_port(session_id, [tool_name.to_owned()]);
        let result = port
            .call(
                "read",
                tool_name,
                json!({"id": "n-1"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.result.success);
        assert_eq!(result.result.output, json!({"ok": true}));
        assert!(result.observation.contains("ok"));
        assert!(executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn workflow_child_calls_receive_unique_canonical_step_ids() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        allow_opaque_capabilities_for_test(&supervisor).await;
        let session_id = "ses-42424242424242424242424242424242";
        let tool_name = "mcp__notes__capture";
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(ChildStepCaptureTool {
                    name: tool_name.into(),
                    captured: captured.clone(),
                }),
            )
            .await;

        let request = supervisor
            .authorization
            .authorization_request(Some(session_id), tool_name, &json!({}))
            .await
            .unwrap();
        supervisor
            .authorization
            .grant(
                Some(session_id),
                request.policy.capability,
                haven_common::types::PermissionEffect::Allow,
                haven_common::types::PermissionScope::Session,
            )
            .await;

        let port = supervisor.workflow_subcall_port(session_id, [tool_name.to_owned()]);
        for step_id in ["read", "read-again"] {
            port.call(step_id, tool_name, json!({}), CancellationToken::new())
                .await
                .expect("Workflow child call should execute");
        }

        let step_ids = captured.lock().unwrap().clone();
        assert_eq!(step_ids.len(), 2);
        assert!(
            step_ids
                .iter()
                .all(|step_id| haven_common::types::is_canonical_id(step_id, "step"))
        );
        assert_ne!(step_ids[0], step_ids[1]);
    }

    #[tokio::test]
    async fn gated_workflow_routes_each_node_through_agent_and_tools_authorization() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        allow_opaque_capabilities_for_test(&supervisor).await;
        let session_id = "ses-78797979797979797979797979797979";
        let child_name = "mcp__notes__read";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: child_name.into(),
                    risk_level: RiskLevel::Safe,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;
        let request = supervisor
            .authorization
            .authorization_request(Some(session_id), child_name, &json!({"id": "n-1"}))
            .await
            .unwrap();
        supervisor
            .authorization
            .grant(
                Some(session_id),
                request.policy.capability,
                haven_common::types::PermissionEffect::Allow,
                haven_common::types::PermissionScope::Session,
            )
            .await;
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(WorkflowInvocationTestTool {
                    definitions: vec![haven_tools::WorkflowCapabilityDefinition {
                        name: child_name.into(),
                        description: "read a test note".into(),
                        input_schema: json!({"type": "object"}),
                    }],
                }),
            )
            .await;

        let execution = supervisor
            .execute_gated(
                Some(session_id),
                haven_tools::WORKFLOW_RUN_TOOL_NAME,
                json!({
                    "workflow": {
                        "version": 1,
                        "steps": [{
                            "id": "read",
                            "capability": child_name,
                            "input": {"id": {"kind": "literal", "value": "n-1"}}
                        }]
                    }
                }),
                CancellationToken::new(),
                None,
                None,
            )
            .await
            .unwrap();

        assert!(execution.result.success, "{:?}", execution.result.error);
        assert_eq!(execution.result.output["status"], "completed");
        assert_eq!(execution.result.output["completed_step_ids"][0], "read");
        assert!(executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn workflow_confirmation_required_child_fails_before_side_effect() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        allow_opaque_capabilities_for_test(&supervisor).await;
        let session_id = "ses-34353535353535353535353535353535";
        let tool_name = "mcp__notes__write";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;

        let port = supervisor.workflow_subcall_port(session_id, [tool_name.to_owned()]);
        let error = port
            .call(
                "write",
                tool_name,
                json!({"id": "n-1", "text": "change"}),
                CancellationToken::new(),
            )
            .await
            .expect_err("a Workflow child must not inherit outer approval");

        assert!(error.contains("requires its own user confirmation"));
        assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
        let nested = port
            .pending_confirmation()
            .expect("Workflow must retain the exact child confirmation request");
        assert_eq!(nested.call_index, 0);
        let resumed = supervisor
            .workflow_subcall_port_with_resume(
                session_id,
                [tool_name.to_owned()],
                None,
                Some(nested),
            )
            .unwrap();
        let result = resumed
            .call(
                "write",
                tool_name,
                json!({"id": "n-1", "text": "change"}),
                CancellationToken::new(),
            )
            .await
            .expect("the resumed child should execute with its receipt");
        assert!(result.result.success);
        assert!(executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn workflow_subcall_port_rejects_names_outside_its_run_allowlist() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-56575757575757575757575757575757";
        let tool_name = "mcp__notes__read";
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed_by_tool = executed.clone();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::Safe,
                    on_execute: Some(Arc::new(move || {
                        executed_by_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;

        let port = supervisor.workflow_subcall_port(session_id, std::iter::empty());
        let error = port
            .call("read", tool_name, json!({}), CancellationToken::new())
            .await
            .expect_err("an unlisted child must stop before authorization");

        assert!(error.contains("allowlist"));
        assert!(!executed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn session_grant_is_durable_before_scheduled_confirmation_wakes_tool() {
        let (supervisor, tools, database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("grant before confirm wake")
            .await
            .unwrap();
        let tool_name = "scheduled.high";
        let input = json!({"target": "recording"});
        let observed_durable_grant = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = observed_durable_grant.clone();
        let db = database.clone();
        let session_id = session.session_id.clone();
        let observed_session_id = session_id.clone();
        let expected_capability = haven_common::types::CapabilityScope::try_new(
            haven_common::types::permission_key(tool_name, &input),
        )
        .unwrap();
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                &session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: Some(Arc::new(move || {
                        let grants = db
                            .session_authorization_grants(&observed_session_id)
                            .unwrap();
                        observed.store(
                            grants.iter().any(|grant| {
                                grant.capability == expected_capability
                                    && grant.effect == PermissionEffect::Allow
                            }),
                            std::sync::atomic::Ordering::SeqCst,
                        );
                    })),
                    result: None,
                }),
            )
            .await;

        let authorization_request = supervisor
            .scheduled_authorization_request(Some(&session_id), tool_name, &input)
            .await
            .unwrap();
        let receipt = match supervisor
            .authorization
            .authorize(&authorization_request)
            .await
        {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("expected confirmation, got {decision:?}"),
        };
        let tool_run_id =
            running_scheduled_tool_run(&supervisor, Some(&session_id), tool_name).await;
        let confirmation_id = supervisor
            .request_scheduled_confirm(
                &tool_run_id,
                Some(&session_id),
                tool_name,
                input,
                receipt,
                "scheduled confirmation",
            )
            .await
            .unwrap();
        let owner = crate::interaction::InteractionOwner::ScheduledToolRun {
            tool_run_id: tool_run_id.clone(),
        };
        let wrong_owner = crate::interaction::InteractionOwner::ScheduledToolRun {
            tool_run_id: haven_common::types::new_id("toolrun"),
        };
        assert!(
            supervisor
                .resolve_confirmation_for_owner(&wrong_owner, &confirmation_id, true)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            supervisor
                .scheduled_confirmation_request(&tool_run_id, &confirmation_id,)
                .await
                .is_some()
        );
        let wrong_request_id: haven_common::types::ConfirmId =
            haven_common::types::ConfirmId::new();
        assert!(
            supervisor
                .resolve_confirmation_for_owner(&owner, &wrong_request_id, true)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            supervisor
                .scheduled_confirmation_request(&tool_run_id, &confirmation_id,)
                .await
                .is_some()
        );

        assert!(
            supervisor
                .resolve_confirmation_with_session_grant_for_owner(
                    &owner,
                    &confirmation_id,
                    haven_common::types::PermissionTarget::Operation,
                    PermissionEffect::Allow,
                )
                .await
                .unwrap()
                .is_some()
        );

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !observed_durable_grant.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("scheduled tool should observe its durable grant before execution");
    }

    #[tokio::test]
    async fn session_confirmation_route_uses_only_the_declared_actor() {
        let (supervisor, _tools, _database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("owner-routed confirmation")
            .await
            .unwrap();
        let other_session = supervisor
            .create_session("different confirmation owner")
            .await
            .unwrap();
        let request = crate::interaction::InteractionRequest::confirm(
            &session.session_id,
            1,
            "files.write".into(),
            json!({"path": "notes.txt"}),
            "call-owner-route".into(),
            format!("step-{}", "1".repeat(32)),
            0,
            RiskLevel::High,
            Some(future_receipt(
                haven_common::types::ConfirmId::new(),
                "files.write",
            )),
        );
        supervisor
            .request_interaction(request.clone())
            .await
            .unwrap();
        let other_request = crate::interaction::InteractionRequest::confirm(
            &other_session.session_id,
            1,
            "files.write".into(),
            json!({"path": "other-notes.txt"}),
            "call-other-owner-route".into(),
            format!("step-{}", "2".repeat(32)),
            0,
            RiskLevel::High,
            Some(future_receipt(
                haven_common::types::ConfirmId::new(),
                "files.write",
            )),
        );
        supervisor
            .request_interaction(other_request.clone())
            .await
            .unwrap();
        let request_id =
            haven_common::types::ConfirmId::try_from(request.request_id.clone()).unwrap();
        let other_request_id =
            haven_common::types::ConfirmId::try_from(other_request.request_id.clone()).unwrap();
        let wrong_owner = crate::interaction::InteractionOwner::Session {
            session_id: other_session.session_id.clone(),
        };

        assert!(
            supervisor
                .resolve_confirmation_for_owner(&wrong_owner, &request_id, true)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&session.session_id, &request_id)
                .await
                .is_some()
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&other_session.session_id, &other_request_id)
                .await
                .is_some()
        );

        let owner = crate::interaction::InteractionOwner::Session {
            session_id: session.session_id.clone(),
        };
        let resolved = supervisor
            .resolve_confirmation_for_owner(&owner, &request_id, false)
            .await
            .unwrap()
            .expect("the request must resolve through its owning actor");
        assert_eq!(
            resolved.session_id.as_deref(),
            Some(session.session_id.as_str())
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&session.session_id, &request_id)
                .await
                .is_none()
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&other_session.session_id, &other_request_id)
                .await
                .is_some(),
            "resolving one actor must not consume another actor's confirmation"
        );
    }

    #[tokio::test]
    async fn session_grant_survives_resolve_append_failure_and_same_request_retry() {
        let (supervisor, tools, database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("session grant resolve append retry")
            .await
            .unwrap();
        let tool_name = "files.write";
        let input = json!({"path": "notes.txt"});
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                &session.session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: None,
                    result: None,
                }),
            )
            .await;

        let authorization_request = supervisor
            .scheduled_authorization_request(Some(&session.session_id), tool_name, &input)
            .await
            .unwrap();
        let receipt = match supervisor
            .authorization
            .authorize(&authorization_request)
            .await
        {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("expected confirmation, got {decision:?}"),
        };
        let request_id = receipt.confirmation_id.clone();
        let step_id = format!("step-{}", "a".repeat(32));
        let request = crate::interaction::InteractionRequest::confirm(
            &session.session_id,
            1,
            tool_name.into(),
            input,
            "call-grant-resolve-retry".into(),
            step_id,
            0,
            RiskLevel::High,
            Some(receipt),
        );
        supervisor
            .request_confirm_batch(&session.session_id, vec![request])
            .await
            .unwrap();
        assert_eq!(
            supervisor
                .get_active_session_status(&session.session_id)
                .await,
            Some(haven_common::lifecycle::SessionStatus::Paused)
        );

        database
            .conn()
            .execute_batch(
                "CREATE TRIGGER reject_confirmation_resolve_event
                 BEFORE INSERT ON session_events
                 WHEN NEW.event_type = 'interaction_resolved'
                 BEGIN SELECT RAISE(ABORT, 'test confirmation resolve append failure'); END;",
            )
            .unwrap();
        let owner = crate::interaction::InteractionOwner::Session {
            session_id: session.session_id.clone(),
        };
        let error = supervisor
            .resolve_confirmation_with_session_grant_for_owner(
                &owner,
                &request_id,
                haven_common::types::PermissionTarget::Operation,
                PermissionEffect::Allow,
            )
            .await
            .expect_err("a failed durable decision append must be retryable");
        assert!(format!("{error:#}").contains("test confirmation resolve append failure"));

        let grants = database
            .session_authorization_grants(&session.session_id)
            .unwrap();
        assert_eq!(grants.len(), 1, "the approved grant remains durable");
        assert_eq!(grants[0].effect, PermissionEffect::Allow);
        assert!(matches!(
            supervisor
                .authorization
                .authorize(&authorization_request)
                .await,
            AuthorizationDecision::AutoApproved
        ));
        assert!(
            supervisor
                .pending_session_confirmation_request(&session.session_id, &request_id)
                .await
                .is_some(),
            "the original actor must retain the pending request after append failure"
        );
        assert_eq!(
            supervisor
                .get_active_session_status(&session.session_id)
                .await,
            Some(haven_common::lifecycle::SessionStatus::Paused),
            "append failure must not wake the paused session"
        );
        let events = supervisor
            .store
            .read_active_domain_events_async(&session.session_id)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == haven_memory::INTERACTION_REQUESTED_EVENT_TYPE)
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .all(|event| event.event_type != haven_memory::INTERACTION_RESOLVED_EVENT_TYPE),
            "the failed append must not leave a partial decision event"
        );

        database
            .conn()
            .execute_batch("DROP TRIGGER reject_confirmation_resolve_event;")
            .unwrap();
        assert!(
            supervisor
                .resolve_confirmation_with_session_grant_for_owner(
                    &owner,
                    &request_id,
                    haven_common::types::PermissionTarget::Operation,
                    PermissionEffect::Allow,
                )
                .await
                .unwrap()
                .is_some(),
            "the same request must resolve after the durable store recovers"
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&session.session_id, &request_id)
                .await
                .is_none()
        );
        assert_eq!(
            supervisor
                .get_active_session_status(&session.session_id)
                .await,
            Some(haven_common::lifecycle::SessionStatus::Pending)
        );
        assert_eq!(
            database
                .session_authorization_grants(&session.session_id)
                .unwrap()
                .len(),
            1,
            "retry must not duplicate the already durable grant"
        );
        let events = supervisor
            .store
            .read_active_domain_events_async(&session.session_id)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == haven_memory::INTERACTION_RESOLVED_EVENT_TYPE)
                .count(),
            1,
            "a successful retry must append exactly one decision event"
        );
    }

    #[tokio::test]
    async fn scheduled_resolve_and_expiry_share_a_single_terminal_claim() {
        let (supervisor, _tools, _database, _directory) = test_supervisor();
        let tool_run_id = running_scheduled_tool_run(&supervisor, None, "files.write").await;
        let confirmation_id: haven_common::types::ConfirmId = haven_common::types::ConfirmId::new();
        supervisor
            .request_scheduled_confirm(
                &tool_run_id,
                None,
                "files.write",
                json!({"path": "notes.txt"}),
                future_receipt(confirmation_id.clone(), "files.write"),
                "scheduled confirmation",
            )
            .await
            .unwrap();
        let owner = crate::interaction::InteractionOwner::ScheduledToolRun {
            tool_run_id: tool_run_id.clone(),
        };

        let resolve_supervisor = supervisor.clone();
        let resolve_owner = owner.clone();
        let resolve_id = confirmation_id.clone();
        let resolve = tokio::spawn(async move {
            resolve_supervisor
                .resolve_confirmation_for_owner(&resolve_owner, &resolve_id, true)
                .await
                .unwrap()
        });
        let expiry_supervisor = supervisor.clone();
        let expiry_id = confirmation_id.clone();
        let expiry_tool_run_id = tool_run_id.clone();
        let expiry = tokio::spawn(async move {
            expiry_supervisor
                .expire_scheduled_confirmation(&expiry_tool_run_id, &expiry_id)
                .await
                .unwrap()
        });

        let outcomes = [resolve.await.unwrap(), expiry.await.unwrap()];
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_some()).count(),
            1
        );
        assert!(
            supervisor
                .scheduled_confirmation_request(&tool_run_id, &confirmation_id)
                .await
                .is_none()
        );
        assert!(
            supervisor
                .resolve_confirmation_for_owner(&owner, &confirmation_id, true)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn scheduled_cancel_winning_confirmation_prevents_grant_and_execution() {
        let (supervisor, tools, database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("cancel wins scheduled confirmation")
            .await
            .unwrap();
        let tool_name = "scheduled.critical";
        let input = json!({"target": "recording"});
        let tool_executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed = Arc::clone(&tool_executed);
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(
                &session.session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: Some(Arc::new(move || {
                        executed.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                    result: None,
                }),
            )
            .await;
        let authorization_request = supervisor
            .scheduled_authorization_request(Some(&session.session_id), tool_name, &input)
            .await
            .unwrap();
        let receipt = match supervisor
            .authorization
            .authorize(&authorization_request)
            .await
        {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("expected confirmation, got {decision:?}"),
        };
        let tool_run_id =
            running_scheduled_tool_run(&supervisor, Some(&session.session_id), tool_name).await;
        let confirmation_id = supervisor
            .request_scheduled_confirm(
                &tool_run_id,
                Some(&session.session_id),
                tool_name,
                input,
                receipt,
                "scheduled confirmation",
            )
            .await
            .unwrap();
        let owner = crate::interaction::InteractionOwner::ScheduledToolRun {
            tool_run_id: tool_run_id.clone(),
        };

        assert!(
            supervisor
                .tool_run_test_support()
                .cancel(&tool_run_id)
                .await
        );
        assert!(
            supervisor
                .resolve_confirmation_with_session_grant_for_owner(
                    &owner,
                    &confirmation_id,
                    haven_common::types::PermissionTarget::Operation,
                    PermissionEffect::Allow,
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            database
                .session_authorization_grants(&session.session_id)
                .unwrap()
                .is_empty()
        );
        assert!(!tool_executed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            supervisor
                .scheduled_confirmation_request(&tool_run_id, &confirmation_id)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_grant_loses_to_an_already_queued_once_rejection_without_stale_row() {
        let (supervisor, _tools, database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("serialized confirmation decisions")
            .await
            .unwrap();
        let confirmation_id: haven_common::types::ConfirmId = haven_common::types::ConfirmId::new();
        supervisor
            .request_scheduled_confirm(
                "toolrun-00000000000000000000000000000002",
                Some(&session.session_id),
                "files.write",
                json!({"path": "notes.txt"}),
                future_receipt(confirmation_id.clone(), "files.write"),
                "scheduled confirmation",
            )
            .await
            .unwrap();

        // Hold the shared resolver gate, then queue a one-shot rejection
        // before a duplicate Session-Allow click. FIFO serialization must let
        // the rejection consume the pending request before the grant path can
        // inspect or persist it.
        let gate = supervisor.confirmation_resolution_gate.clone();
        let held_gate = gate.lock().await;
        let owner = crate::interaction::InteractionOwner::ScheduledToolRun {
            tool_run_id: "toolrun-00000000000000000000000000000002".into(),
        };
        let once_supervisor = supervisor.clone();
        let once_id = confirmation_id.clone();
        let (once_started_tx, once_started_rx) = tokio::sync::oneshot::channel();
        let once = tokio::spawn(async move {
            let _ = once_started_tx.send(());
            once_supervisor
                .resolve_confirmation_for_owner(&owner, &once_id, false)
                .await
        });
        once_started_rx.await.unwrap();

        let session_supervisor = supervisor.clone();
        let session_id_for_call = confirmation_id.clone();
        let (session_started_tx, session_started_rx) = tokio::sync::oneshot::channel();
        let session_grant = tokio::spawn(async move {
            let _ = session_started_tx.send(());
            session_supervisor
                .resolve_confirmation_with_session_grant_for_owner(
                    &crate::interaction::InteractionOwner::ScheduledToolRun {
                        tool_run_id: "toolrun-00000000000000000000000000000002".into(),
                    },
                    &session_id_for_call,
                    haven_common::types::PermissionTarget::Operation,
                    PermissionEffect::Allow,
                )
                .await
        });
        session_started_rx.await.unwrap();
        drop(held_gate);

        assert!(once.await.unwrap().unwrap().is_some());
        assert!(session_grant.await.unwrap().unwrap().is_none());
        assert!(
            database
                .session_authorization_grants(&session.session_id)
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod tool_step_persistence_tests {
    use super::*;
    use haven_memory::Database;
    use serde_json::json;

    #[tokio::test]
    async fn tool_step_lifecycle_persists_identity_through_session_store() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let supervisor = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        let session = supervisor
            .create_session("action step store port")
            .await
            .unwrap();
        let step_id = haven_common::types::new_id("step");
        let input = json!({"path": "notes.txt", "silent": true});

        supervisor
            .begin_tool_step_with_identity(
                &session.session_id,
                "files.read",
                &input,
                5,
                3,
                Some("provider-call-5"),
                &step_id,
            )
            .await
            .unwrap();

        let pending = db.list_session_steps(&session.session_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].step_id, step_id);
        assert_eq!(pending[0].step_number, 5);
        assert_eq!(pending[0].tool_index, 3);
        assert_eq!(pending[0].tool_name.as_deref(), Some("files.read"));
        assert_eq!(pending[0].tool_call_id.as_deref(), Some("provider-call-5"));
        assert!(pending[0].silent);
        assert!(!pending[0].is_high_risk);
        assert_eq!(
            pending[0].status,
            haven_common::lifecycle::SessionStepStatus::Pending
        );

        supervisor
            .start_tool_step_with_identity(
                &session.session_id,
                "files.read",
                &input,
                5,
                3,
                Some("provider-call-5"),
                &step_id,
            )
            .await
            .unwrap();
        supervisor
            .finish_step_with_outcome(
                &session.session_id,
                "files.read",
                &input,
                5,
                3,
                Some("provider-call-5"),
                &step_id,
                "cancelled during execution",
                ToolStepOutcome::Cancelled,
            )
            .await;

        let finished = db.list_session_steps(&session.session_id).unwrap();
        assert_eq!(finished.len(), 1);
        assert_eq!(
            finished[0].status,
            haven_common::lifecycle::SessionStepStatus::Cancelled
        );
        assert_eq!(
            finished[0].observation.as_deref(),
            Some("cancelled during execution")
        );
        assert!(finished[0].started_at.is_some());
        assert!(finished[0].completed_at.is_some());

        let replay = supervisor
            .start_tool_step_with_identity(
                &session.session_id,
                "files.read",
                &input,
                5,
                3,
                Some("provider-call-5"),
                &step_id,
            )
            .await
            .expect_err("a terminal tool step must not start its side effect again");
        assert!(replay.to_string().contains("already terminal"));
    }
}

#[cfg(test)]
mod interaction_owner_route_tests {
    use super::*;
    use serde_json::json;

    fn future_receipt(capability: &str) -> haven_tools::ConfirmationReceipt {
        haven_tools::ConfirmationReceipt {
            confirmation_id: haven_common::types::ConfirmId::new(),
            capability: haven_common::types::CapabilityScope::try_new(capability).unwrap(),
            canonical_input_hash: String::new(),
            effective_risk: haven_common::types::RiskLevel::High,
            policy_revision: 1,
            expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
        }
    }

    #[test]
    fn owner_must_match_the_pending_request_kind_and_route_key() {
        let session_id = "ses-1234567890abcdef1234567890abcdef";
        let session_request = crate::interaction::InteractionRequest::confirm(
            session_id,
            1,
            "files.write".into(),
            json!({"path": "notes.txt"}),
            "call-session".into(),
            "step-9c66573ca9c921cc5c45210d8ad159cc".into(),
            0,
            haven_common::types::RiskLevel::High,
            Some(future_receipt("files.write")),
        );
        let session_owner = crate::interaction::InteractionOwner::Session {
            session_id: session_id.into(),
        };
        assert!(interaction_owner_matches_request(
            &session_owner,
            &session_request
        ));
        assert!(!interaction_owner_matches_request(
            &crate::interaction::InteractionOwner::Session {
                session_id: "ses-9589b11fb124d96caa27b5509dadcf99".into()
            },
            &session_request
        ));
        assert!(!interaction_owner_matches_request(
            &crate::interaction::InteractionOwner::AppCommand,
            &session_request
        ));

        let tool_run_id = "toolrun-1234567890abcdef1234567890abcdef";
        let receipt = future_receipt("files.write");
        let scheduled_request = crate::interaction::InteractionRequest::scheduled_confirm(
            tool_run_id.into(),
            Some(session_id),
            "files.write".into(),
            json!({"path": "notes.txt"}),
            receipt,
            "scheduled".into(),
        );
        assert!(interaction_owner_matches_request(
            &crate::interaction::InteractionOwner::ScheduledToolRun {
                tool_run_id: tool_run_id.into()
            },
            &scheduled_request
        ));
        assert!(!interaction_owner_matches_request(
            &session_owner,
            &scheduled_request
        ));
    }
}
