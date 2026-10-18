//! ToolsFacade adapters owned by the application composition boundary.

use std::sync::Arc;

use async_trait::async_trait;
use haven_agent::{
    AgentSchedulePort, AgentToolPorts, AgentToolRunPort, ManagedAssetLeasePort,
    PromptCatalogContent, PromptCatalogVersions, PromptRuntimeContext, PromptToolPort,
    ScheduleFiredReceiverPort, SessionToolOverlayPort, SessionToolPorts, ToolAuthorizationPort,
    ToolCatalogPort, ToolExecutionContext, ToolExecutionPort, ToolObservationPort,
    ToolRunCompletionReceiverPort,
};
use haven_common::types::{MessageAttachment, RiskLevel};
use haven_tools::{
    AuthorizationDecision, AuthorizationPort, AuthorizationRequest, ConfirmationReceipt,
    ScheduleAgentCapability, ScheduleFiredReceiver, SkillCatalogPort, ToolCatalogSnapshot,
    ToolRegistration, ToolResult, ToolRunAgentCapability, ToolRunCompletion,
    ToolRunCompletionCapability, ToolRunListView, ToolRunRestoreSummary, ToolRunStart, ToolsFacade,
};
use serde_json::Value;

/// Assemble the one shared ToolsFacade into the narrow capabilities consumed
/// by Agent runtime owners. The adapter stays in app-binary so haven-agent
/// depends on ports and tool DTOs, never on the Tools facade.
pub(crate) fn agent_tool_ports_from_capabilities(
    tools: Arc<ToolsFacade>,
    authorization: Arc<dyn AuthorizationPort>,
    skills: Arc<dyn SkillCatalogPort>,
    tool_runs: Arc<dyn ToolRunAgentCapability>,
    schedules: Arc<dyn ScheduleAgentCapability>,
) -> AgentToolPorts {
    let adapter = Arc::new(ToolsFacadeAgentAdapter {
        tools,
        authorization,
        skills,
        tool_runs,
        schedules,
    });

    let prompt: Arc<dyn PromptToolPort> = adapter.clone();
    let catalog: Arc<dyn ToolCatalogPort> = adapter.clone();
    let session = SessionToolPorts::new(
        adapter.clone(),
        adapter.clone(),
        Arc::clone(&catalog),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
    );

    AgentToolPorts::new(prompt, catalog, session)
}

#[cfg(test)]
fn agent_tool_ports_from_facade(tools: Arc<ToolsFacade>) -> AgentToolPorts {
    let services = tools.share_services();
    agent_tool_ports_from_capabilities(
        tools,
        services.authorization,
        services.skills,
        services.tool_run_agent,
        services.schedule_agent,
    )
}

struct ToolsFacadeAgentAdapter {
    tools: Arc<ToolsFacade>,
    authorization: Arc<dyn AuthorizationPort>,
    skills: Arc<dyn SkillCatalogPort>,
    tool_runs: Arc<dyn ToolRunAgentCapability>,
    schedules: Arc<dyn ScheduleAgentCapability>,
}

impl ToolsFacadeAgentAdapter {
    async fn validate_toolrun_target(
        &self,
        session_id: Option<&str>,
        target: &ToolRunStart,
    ) -> Result<(), String> {
        let session_id =
            session_id.ok_or_else(|| "toolrun.start requires a live session context".to_owned())?;
        let catalog = self.tools.tool_catalog_snapshot(session_id).await;
        let Some(validation) = catalog.validate_input(&target.tool_name, &target.input) else {
            return Err(format!(
                "toolrun.start target '{}' is not enabled in this session",
                target.tool_name
            ));
        };
        validation.map_err(|error| {
            format!(
                "toolrun.start target '{}' input is invalid: {error}",
                target.tool_name
            )
        })
    }
}

#[async_trait]
impl PromptToolPort for ToolsFacadeAgentAdapter {
    fn catalog_versions(&self) -> PromptCatalogVersions {
        PromptCatalogVersions {
            global_catalog_version: self.tools.catalog_version(),
            mcp_catalog_version: self.tools.mcp_catalog_version(),
            skills_catalog_version: self.skills.catalog_version(),
        }
    }

    async fn catalog_content(&self) -> PromptCatalogContent {
        let builtin_tool_definitions = self.tools.list_enabled_builtin_tool_definitions().await;
        PromptCatalogContent {
            builtin_tool_definitions,
            mcp_index: self.tools.build_mcp_index().await,
            skills: self.skills.list_skill_infos().await,
        }
    }

    async fn runtime_context(&self) -> PromptRuntimeContext {
        let default_shell = self.tools.default_shell_name().await;
        let capabilities = self.tools.runtime_capabilities().await;
        let permission_summary = self.authorization.prompt_summary().await;
        let execution_workspace = self.tools.execution_workspace_snapshot().await;
        PromptRuntimeContext {
            default_shell,
            capabilities,
            permission_summary,
            execution_workspace,
        }
    }
}

#[async_trait]
impl ToolCatalogPort for ToolsFacadeAgentAdapter {
    async fn catalog_snapshot(&self, session_id: &str) -> Arc<ToolCatalogSnapshot> {
        Arc::new(self.tools.tool_catalog_snapshot(session_id).await)
    }
}

#[async_trait]
impl ToolAuthorizationPort for ToolsFacadeAgentAdapter {
    async fn risk_level(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<RiskLevel, String> {
        if tool_name == "toolrun.start" {
            let target = haven_tools::parse_toolrun_start(input)?;
            self.validate_toolrun_target(session_id, &target).await?;
            return self
                .tools
                .resolve_risk_level(session_id, &target.tool_name, &target.input)
                .await;
        }
        self.tools
            .resolve_risk_level(session_id, tool_name, input)
            .await
    }

    async fn authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<AuthorizationRequest, String> {
        if tool_name == "toolrun.start" {
            let target = haven_tools::parse_toolrun_start(input)?;
            self.validate_toolrun_target(session_id, &target).await?;
            return self
                .tools
                .resolve_authorization_request(session_id, &target.tool_name, &target.input)
                .await;
        }
        self.tools
            .resolve_authorization_request(session_id, tool_name, input)
            .await
    }

    async fn authorization_request_for_codemode_subcall(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<AuthorizationRequest, String> {
        if tool_name.starts_with("mcp__") {
            self.tools
                .resolve_codemode_subcall_authorization_request(session_id, tool_name, input)
                .await
        } else {
            self.authorization_request(session_id, tool_name, input)
                .await
        }
    }

    async fn authorization_request_from_catalog(
        &self,
        catalog: &ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Result<AuthorizationRequest, String> {
        if tool_name == "toolrun.start" {
            let target = haven_tools::parse_toolrun_start(input)?;
            if catalog.get(&target.tool_name).is_none() {
                return Err(format!(
                    "toolrun.start target '{}' was not present in the turn catalog",
                    target.tool_name
                ));
            }
            if let Some(validation) = catalog.validate_input(&target.tool_name, &target.input) {
                validation.map_err(|error| {
                    format!(
                        "toolrun.start target '{}' input is invalid: {error}",
                        target.tool_name
                    )
                })?;
            }
            return self
                .tools
                .resolve_authorization_request_from_snapshot(
                    catalog,
                    session_id,
                    &target.tool_name,
                    &target.input,
                )
                .await;
        }
        self.tools
            .resolve_authorization_request_from_snapshot(catalog, session_id, tool_name, input)
            .await
    }

    async fn authorize(&self, request: &AuthorizationRequest) -> AuthorizationDecision {
        self.authorization.authorize(request).await
    }

    async fn authorize_for_execution(
        &self,
        request: &AuthorizationRequest,
    ) -> haven_tools::AuthorizationCheck {
        self.authorization.authorize_for_execution(request).await
    }

    async fn verify_receipt(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<(), String> {
        self.authorization.verify_receipt(request, receipt).await
    }

    async fn confirm_for_execution(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<haven_tools::AuthorizationExecutionProof, String> {
        self.authorization
            .confirm_for_execution(request, receipt)
            .await
    }

    async fn verify_execution_proof(
        &self,
        request: &AuthorizationRequest,
        proof: &haven_tools::AuthorizationExecutionProof,
    ) -> Result<(), String> {
        self.authorization
            .verify_execution_proof(request, proof)
            .await
    }

    async fn grant(
        &self,
        session_id: Option<&str>,
        capability: haven_common::types::CapabilityScope,
        effect: haven_common::types::PermissionEffect,
        scope: haven_common::types::PermissionScope,
    ) {
        self.authorization
            .grant(session_id, capability, effect, scope)
            .await;
    }

    async fn apply_security(&self, security: &haven_common::config::SecurityConfig) {
        if let Err(error) = self.authorization.apply_security(security).await {
            tracing::error!(%error, "security configuration applied with execution workspace unavailable");
        }
    }

    async fn set_permission_mode(&self, mode: haven_common::types::PermissionMode) {
        self.authorization.set_permission_mode(mode).await;
    }

    async fn set_boundaries(
        &self,
        sandbox_mode: haven_common::types::SandboxMode,
        writable_roots: Vec<std::path::PathBuf>,
        network_policy: haven_common::types::NetworkPolicy,
    ) {
        self.authorization
            .set_boundaries(sandbox_mode, writable_roots, network_policy)
            .await;
    }

    async fn list_permanent(&self) -> Vec<haven_common::config::StoredPermission> {
        self.authorization.list_permanent().await
    }

    async fn revoke_permanent(&self, capability: &str) -> bool {
        self.authorization.revoke_permanent(capability).await
    }

    async fn revoke_session_grant(
        &self,
        session_id: &str,
        capability: &haven_common::types::CapabilityScope,
    ) -> bool {
        self.authorization
            .revoke_session_grant(session_id, capability)
            .await
    }

    async fn clear_permanent(&self) -> usize {
        self.authorization.clear_permanent().await
    }

    async fn clear_session_trust(&self, session_id: &str) {
        self.authorization.clear_session_trust(session_id).await;
    }

    async fn clear_all_trust(&self) {
        self.authorization.clear_all_trust().await;
    }

    async fn prompt_summary(&self) -> String {
        self.authorization.prompt_summary().await
    }
}

#[async_trait]
impl AgentToolRunPort for ToolsFacadeAgentAdapter {
    async fn attach_session(&self, tool_run_id: &str, session_id: &str) {
        self.tool_runs.attach_session(tool_run_id, session_id).await;
    }

    async fn cancel_owned_background_by_session_checked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<()> {
        self.tool_runs
            .cancel_owned_background_by_session_checked(session_id)
            .await
    }

    async fn cancel_owned_background_by_session(&self, session_id: &str) {
        self.tool_runs
            .cancel_owned_background_by_session(session_id)
            .await;
    }

    async fn list_for_session_views(&self, session_id: &str) -> Vec<ToolRunListView> {
        self.tool_runs.list_for_session_views(session_id).await
    }

    async fn restore(&self) -> ToolRunRestoreSummary {
        self.tool_runs.restore().await
    }

    async fn acknowledge_completion(&self, tool_run_result_id: &str) {
        self.tool_runs
            .acknowledge_completion(tool_run_result_id)
            .await;
    }

    async fn acknowledge_unowned_completion(&self, tool_run_result_id: &str) {
        self.tool_runs
            .acknowledge_unowned_completion(tool_run_result_id)
            .await;
    }

    fn take_completion_receiver(&self) -> Option<Box<dyn ToolRunCompletionReceiverPort>> {
        let receiver = self.tool_runs.take_completion_receiver()?;
        Some(Box::new(ToolsFacadeToolRunCompletionAdapter { receiver }))
    }
}

#[async_trait]
impl AgentSchedulePort for ToolsFacadeAgentAdapter {
    async fn restore(&self) -> anyhow::Result<haven_tools::ScheduleRestoreSummary> {
        self.schedules.restore().await
    }

    async fn cancel_owned_by_session_checked(&self, session_id: &str) -> anyhow::Result<()> {
        self.schedules
            .cancel_owned_by_session_checked(session_id)
            .await
    }

    async fn claim_execution(&self, schedule_run_id: &str, claim_id: &str) -> anyhow::Result<bool> {
        self.schedules
            .claim_execution(schedule_run_id, claim_id)
            .await
    }

    async fn release_execution(
        &self,
        schedule_run_id: &str,
        claim_id: &str,
    ) -> anyhow::Result<bool> {
        self.schedules
            .release_execution(schedule_run_id, claim_id)
            .await
    }

    async fn finish(
        &self,
        schedule_run_id: &str,
        status: haven_common::lifecycle::ScheduleRunStatus,
        result_summary: Option<&str>,
        error_reason: Option<&str>,
    ) -> anyhow::Result<bool> {
        self.schedules
            .finish(schedule_run_id, status, result_summary, error_reason)
            .await
    }

    fn take_fired_receiver(&self) -> Option<Box<dyn ScheduleFiredReceiverPort>> {
        Some(Box::new(ToolsFacadeScheduleReceiverPortAdapter {
            receiver: self.schedules.take_fired_receiver()?,
        }))
    }

    async fn shutdown(&self) {
        self.schedules.shutdown().await;
    }
}

struct ToolsFacadeScheduleReceiverPortAdapter {
    receiver: Box<dyn ScheduleFiredReceiver>,
}

#[async_trait]
impl ScheduleFiredReceiverPort for ToolsFacadeScheduleReceiverPortAdapter {
    async fn recv(&mut self) -> Option<haven_tools::ScheduleRunTriggered> {
        self.receiver.recv().await
    }
}

struct ToolsFacadeToolRunCompletionAdapter {
    receiver: Box<dyn ToolRunCompletionCapability>,
}

#[async_trait]
impl ToolRunCompletionReceiverPort for ToolsFacadeToolRunCompletionAdapter {
    async fn recv_result_with_recovery(&mut self) -> Option<ToolRunCompletion> {
        self.receiver.recv_result_with_recovery().await
    }
}

#[async_trait]
impl ToolExecutionPort for ToolsFacadeAgentAdapter {
    async fn execute(&self, context: ToolExecutionContext) -> anyhow::Result<ToolResult> {
        let proof = context
            .authorization_proof
            .ok_or_else(|| anyhow::anyhow!("tool execution is missing an authorization proof"))?;
        self.tools
            .execute_authorized_tool_with_step(
                context.session_id.as_deref(),
                &context.tool_name,
                context.input,
                context.cancel,
                context.step_id.as_deref(),
                proof,
            )
            .await
    }

    async fn start_background_tool(
        &self,
        context: ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        let proof = context.authorization_proof.ok_or_else(|| {
            anyhow::anyhow!("background ToolRun is missing the target tool's authorization proof")
        })?;
        let tool_name = context.tool_name;
        let input = context.input;
        let session_id = context
            .session_id
            .ok_or_else(|| anyhow::anyhow!("background ToolRun requires an owning session"))?;
        let source_step_id = context.step_id;
        let execution_tools = Arc::clone(&self.tools);
        let execution_session_id = session_id.clone();
        let execution_tool_name = tool_name.clone();
        let execution_input = input.clone();
        let execution_step_id = source_step_id.clone();
        let invocation: haven_tools::ToolRunInvocation = Box::new(move |cancel| {
            Box::pin(async move {
                execution_tools
                    .execute_authorized_tool_with_step(
                        Some(&execution_session_id),
                        &execution_tool_name,
                        execution_input,
                        cancel,
                        execution_step_id.as_deref(),
                        proof,
                    )
                    .await
            })
        });
        let tool_run_id = self
            .tool_runs
            .start_background(
                tool_name.clone(),
                source_step_id.clone(),
                context.cancel,
                invocation,
            )
            .await?;
        let mut output = haven_common::tools::background_wait_object(
            std::iter::once(tool_run_id.clone()),
            "Background tool call started. If no independent foreground work remains, end the turn; the result will be delivered automatically.",
        );
        output.insert("tool_run_id".into(), serde_json::json!(tool_run_id));
        output.insert("tool_name".into(), serde_json::json!(tool_name));
        output.insert("status".into(), serde_json::json!("running"));
        if let Some(source_step_id) = source_step_id {
            output.insert("source_step_id".into(), serde_json::json!(source_step_id));
        }
        Ok(ToolResult::ok(Value::Object(output)))
    }

    async fn execute_codemode(
        &self,
        context: ToolExecutionContext,
        subcalls: Arc<dyn haven_tools::CodeModeSubcallPort>,
    ) -> anyhow::Result<ToolResult> {
        let proof = context.authorization_proof.ok_or_else(|| {
            anyhow::anyhow!("CodeMode execution is missing an authorization proof")
        })?;
        self.tools
            .execute_authorized_codemode_tool_with_step(
                haven_tools::AuthorizedCodeModeToolExecution {
                    session_id: context.session_id,
                    tool_name: context.tool_name,
                    input: context.input,
                    cancel: context.cancel,
                    step_id: context.step_id,
                    proof,
                    subcalls,
                },
            )
            .await
    }

    async fn execute_codemode_subcall(
        &self,
        context: ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        let proof = context
            .authorization_proof
            .ok_or_else(|| anyhow::anyhow!("CodeMode child is missing an authorization proof"))?;
        if context.tool_name.starts_with("mcp__") {
            self.tools
                .execute_authorized_codemode_subcall_with_step(
                    haven_tools::AuthorizedCodeModeSubcallExecution {
                        session_id: context.session_id,
                        tool_name: context.tool_name,
                        input: context.input,
                        cancel: context.cancel,
                        step_id: context.step_id,
                        proof,
                    },
                )
                .await
        } else {
            self.tools
                .execute_authorized_tool_with_step(
                    context.session_id.as_deref(),
                    &context.tool_name,
                    context.input,
                    context.cancel,
                    context.step_id.as_deref(),
                    proof,
                )
                .await
        }
    }

    async fn registrations(
        &self,
        session_id: &str,
        tool_name: &str,
        output: &Value,
    ) -> Vec<ToolRegistration> {
        self.tools
            .get_tool_for_session(Some(session_id), tool_name)
            .await
            .map(|tool| tool.registrations(output))
            .unwrap_or_default()
    }
}

#[async_trait]
impl ToolObservationPort for ToolsFacadeAgentAdapter {
    async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.tools.observation_text(tool_name, result).await
    }
}

impl ManagedAssetLeasePort for ToolsFacadeAgentAdapter {
    fn register_for_session(&self, session_id: &str, attachments: &[MessageAttachment]) {
        self.tools
            .register_managed_assets_for_session(session_id, attachments);
    }

    fn release_for_session(&self, session_id: &str) {
        self.tools.release_managed_assets_for_session(session_id);
    }
}

#[async_trait]
impl SessionToolOverlayPort for ToolsFacadeAgentAdapter {
    async fn unregister_session(&self, session_id: &str) {
        self.tools.unregister_session(session_id).await;
    }

    async fn register_mcp_for_session(
        &self,
        session_id: &str,
        server_name: &str,
        tool_names: Option<&[String]>,
    ) -> bool {
        self.tools
            .register_mcp_for_session(session_id, server_name, tool_names)
            .await
    }

    async fn load_skill_for_session(&self, session_id: &str, names: Vec<String>) -> bool {
        self.tools.load_skill_for_session(session_id, names).await
    }

    async fn load_builtin_operations_for_session(
        &self,
        session_id: &str,
        operations: Option<Vec<String>>,
        roots: Option<Vec<String>>,
    ) -> bool {
        self.tools
            .load_builtin_operations_for_session(session_id, operations, roots)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_agent::SessionSupervisor;
    use haven_common::config::ToolConfig;
    use haven_common::types::RiskLevel;
    use haven_memory::{Database, SessionStore};
    use haven_tools::{Tool, ToolHandle};
    use serde_json::json;
    use std::collections::HashMap;
    use tokio_util::sync::CancellationToken;

    struct ExecutionContextProbe;

    struct EnabledShellProbe;

    #[async_trait]
    impl Tool for EnabledShellProbe {
        fn name(&self) -> String {
            "shell".into()
        }

        fn description(&self) -> String {
            "test shell target schema".into()
        }

        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::High
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
                "additionalProperties": false
            })
        }

        async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({ "ok": true })))
        }
    }

    #[async_trait]
    impl Tool for ExecutionContextProbe {
        fn name(&self) -> String {
            "execution.context_probe".into()
        }

        fn description(&self) -> String {
            "records trusted execution context fields".into()
        }

        fn risk_level(&self, _: &Value) -> RiskLevel {
            RiskLevel::Safe
        }

        fn input_schema(&self) -> Value {
            json!({"type":"object"})
        }

        fn requires_session_id(&self) -> bool {
            true
        }

        fn supports_live_output(&self) -> bool {
            true
        }

        async fn execute(
            &self,
            input: Value,
            cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({
                "input": input,
                "cancelled": cancel.is_cancelled()
            })))
        }
    }

    #[tokio::test]
    async fn composition_adapter_forwards_trusted_execution_context() {
        let tools = Arc::new(ToolsFacade::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tool: ToolHandle = Arc::new(ExecutionContextProbe);
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(session_id, tool)
            .await;
        let ports = agent_tool_ports_from_facade(Arc::clone(&tools));
        let supervisor = SessionSupervisor::new(
            SessionStore::new(Arc::new(Database::open_in_memory().unwrap())),
            ports.session_ports(),
            1,
        );
        let cancel = CancellationToken::new();

        let result = supervisor
            .execute_gated(
                Some(session_id),
                "execution.context_probe",
                json!({
                    "value": "kept",
                    "_session_id": "ses-forged",
                    "_step_id": "step-forged"
                }),
                cancel,
                None,
                Some("step-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            )
            .await
            .unwrap();

        assert_eq!(
            result.result.output,
            json!({
                "input": {
                    "value": "kept",
                    "_session_id": session_id,
                    "_step_id": "step-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                },
                "cancelled": false
            })
        );
    }

    #[tokio::test]
    async fn toolrun_start_uses_enabled_target_schema_and_authorization() {
        let tools = Arc::new(ToolsFacade::new());
        let services = tools.share_services();
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        services
            .tool_catalog_test_support
            .register_session_tool(session_id, Arc::new(EnabledShellProbe))
            .await;
        let adapter = ToolsFacadeAgentAdapter {
            tools: Arc::clone(&tools),
            authorization: services.authorization,
            skills: services.skills,
            tool_runs: services.tool_run_agent,
            schedules: services.schedule_agent,
        };
        let catalog = tools.tool_catalog_snapshot(session_id).await;
        let input = json!({
            "tool_name": "shell",
            "input": {"command": "echo authorized"}
        });

        let risk = adapter
            .risk_level(Some(session_id), "toolrun.start", &input)
            .await
            .unwrap();
        assert_eq!(risk, RiskLevel::High);
        let request = adapter
            .authorization_request_from_catalog(&catalog, Some(session_id), "toolrun.start", &input)
            .await
            .unwrap();
        assert_eq!(request.tool_name, "shell");
        assert_eq!(request.input, json!({"command": "echo authorized"}));

        let invalid = json!({"tool_name": "shell", "input": {}});
        assert!(
            adapter
                .authorization_request_from_catalog(
                    &catalog,
                    Some(session_id),
                    "toolrun.start",
                    &invalid,
                )
                .await
                .is_err()
        );
        let unavailable = json!({"tool_name": "disabled.example", "input": {}});
        assert!(
            adapter
                .authorization_request_from_catalog(
                    &catalog,
                    Some(session_id),
                    "toolrun.start",
                    &unavailable,
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn composition_adapter_forwards_cancellation() {
        let tools = Arc::new(ToolsFacade::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tool: ToolHandle = Arc::new(ExecutionContextProbe);
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(session_id, tool)
            .await;
        let ports = agent_tool_ports_from_facade(tools);
        let supervisor = SessionSupervisor::new(
            SessionStore::new(Arc::new(Database::open_in_memory().unwrap())),
            ports.session_ports(),
            1,
        );
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = supervisor
            .execute_gated(
                Some(session_id),
                "execution.context_probe",
                json!({"value":"kept"}),
                cancel,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            result.result.outcome,
            haven_common::tools::ToolExecutionOutcome::Cancelled
        );
    }

    #[tokio::test]
    async fn prompt_adapter_uses_only_the_published_builtin_catalog() {
        let tools = Arc::new(ToolsFacade::new());
        let catalog_version_before_registration = tools.catalog_version();
        let tool: ToolHandle = Arc::new(ExecutionContextProbe);
        tools
            .share_services()
            .tool_catalog_test_support
            .register_installed_tool(tool)
            .await
            .unwrap();
        assert!(
            tools
                .share_services()
                .tool_catalog_test_support
                .contains_installed_tool("execution.context_probe")
                .await
        );
        assert_eq!(
            tools.catalog_version(),
            catalog_version_before_registration,
            "test registry entries do not publish a provider catalog generation"
        );
        let services = tools.share_services();
        let adapter = ToolsFacadeAgentAdapter {
            tools: Arc::clone(&tools),
            authorization: Arc::clone(&services.authorization),
            skills: Arc::clone(&services.skills),
            tool_runs: Arc::clone(&services.tool_run_agent),
            schedules: Arc::clone(&services.schedule_agent),
        };

        let content = adapter.catalog_content().await;

        assert!(
            content
                .builtin_tool_definitions
                .iter()
                .all(|definition| definition.name != "execution.context_probe"),
            "registry entries do not enter the prompt until the builtin catalog publishes them"
        );
        assert_eq!(
            adapter.catalog_versions().global_catalog_version,
            tools.catalog_version()
        );
    }

    #[tokio::test]
    async fn catalog_and_observation_adapters_preserve_session_and_output_contracts() {
        let tools = Arc::new(ToolsFacade::new());
        let services = tools.share_services();
        let adapter = ToolsFacadeAgentAdapter {
            tools: Arc::clone(&tools),
            authorization: Arc::clone(&services.authorization),
            skills: Arc::clone(&services.skills),
            tool_runs: Arc::clone(&services.tool_run_agent),
            schedules: Arc::clone(&services.schedule_agent),
        };
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tool: ToolHandle = Arc::new(ExecutionContextProbe);
        tools
            .share_services()
            .tool_catalog_test_support
            .register_session_tool(session_id, tool)
            .await;

        let snapshot = adapter.catalog_snapshot(session_id).await;
        assert!(snapshot.get("execution.context_probe").is_some());

        tools
            .set_tool_settings(HashMap::from([(
                "execution.context_probe".into(),
                ToolConfig {
                    max_output_chars: Some(4),
                    ..ToolConfig::default()
                },
            )]))
            .await
            .unwrap();
        assert_eq!(
            adapter
                .observation_text("execution.context_probe", &ToolResult::ok(json!("012345")),)
                .await,
            "0123"
        );
    }
}
