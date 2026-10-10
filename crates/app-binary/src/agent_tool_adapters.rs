//! ToolsFacade adapters owned by the application composition boundary.

use std::sync::Arc;

use async_trait::async_trait;
use haven_agent::{
    AgentToolPorts, AgentToolRunPort, ManagedAssetLeasePort, PromptCatalogContent,
    PromptCatalogVersions, PromptRuntimeContext, PromptToolPort, SessionToolOverlayPort,
    SessionToolPorts, ToolAuthorizationPort, ToolCatalogPort, ToolExecutionContext,
    ToolExecutionPort, ToolObservationPort, ToolRunCompletionReceiverPort,
};
use haven_common::types::{MessageAttachment, RiskLevel};
use haven_memory::ToolRunRow;
use haven_tools::{
    AuthorizationDecision, AuthorizationPort, AuthorizationRequest, ConfirmationReceipt,
    LiveOutputSinkPort, Skill, SkillExecutionPort, ToolCatalogSnapshot, ToolRegistration,
    ToolResult, ToolRunAgentCapability, ToolRunCompletion, ToolRunCompletionCapability,
    ToolRunLifecycleEventSink, ToolRunListView, ToolRunManagementCapability, ToolRunRestoreSummary,
    ToolRunView, ToolsFacade,
};
#[cfg(test)]
use haven_tools::{ScheduledToolRunSpec, ToolRunTestSupportPort};
use serde_json::Value;

/// Assemble the one shared ToolsFacade into the narrow capabilities consumed
/// by Agent runtime owners. The adapter stays in app-binary so haven-agent
/// depends on ports and tool DTOs, never on the Tools facade.
pub(crate) fn agent_tool_ports_from_facade(tools: Arc<ToolsFacade>) -> AgentToolPorts {
    let services = tools.share_services();
    let adapter = Arc::new(ToolsFacadeAgentAdapter {
        tools: Arc::clone(&tools),
        authorization: Arc::clone(&services.authorization),
        tool_runs: Arc::clone(&services.tool_run_agent),
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
        adapter,
    );

    AgentToolPorts::new(prompt, catalog, session)
}

/// App-owned contract for ToolRun IPC and application lifecycle operations.
#[async_trait]
pub(crate) trait AppToolRunPort: Send + Sync {
    fn set_lifecycle_event_sink(&self, sink: ToolRunLifecycleEventSink);

    async fn shutdown(&self);

    async fn board(&self) -> Vec<ToolRunView>;

    async fn list_persisted_tool_runs(&self, kind: Option<&str>)
    -> anyhow::Result<Vec<ToolRunRow>>;

    async fn list_persisted_tool_runs_for_session(
        &self,
        session_id: &str,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<ToolRunRow>>;

    async fn cancel_for_kind(&self, tool_run_id: &str, kind: &str) -> bool;

    async fn delete_terminal(&self, tool_run_id: &str) -> anyhow::Result<bool>;

    async fn clear_terminal_history(&self) -> anyhow::Result<u64>;

    #[cfg(test)]
    async fn schedule(&self, spec: ScheduledToolRunSpec) -> anyhow::Result<String>;
}

pub(crate) fn app_tool_run_port_from_facade(tools: Arc<ToolsFacade>) -> Arc<dyn AppToolRunPort> {
    let services = tools.share_services();
    Arc::new(ToolsFacadeAppToolRunAdapter {
        tool_runs: Arc::clone(&services.tool_run_management),
        #[cfg(test)]
        test_support: Arc::clone(&services.tool_run_test_support),
    })
}

struct ToolsFacadeAppToolRunAdapter {
    tool_runs: Arc<dyn ToolRunManagementCapability>,
    #[cfg(test)]
    test_support: Arc<dyn ToolRunTestSupportPort>,
}

#[async_trait]
impl AppToolRunPort for ToolsFacadeAppToolRunAdapter {
    fn set_lifecycle_event_sink(&self, sink: ToolRunLifecycleEventSink) {
        self.tool_runs.set_lifecycle_event_sink(sink);
    }

    async fn shutdown(&self) {
        self.tool_runs.shutdown().await;
    }

    async fn board(&self) -> Vec<ToolRunView> {
        self.tool_runs.board().await
    }

    async fn list_persisted_tool_runs(
        &self,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        self.tool_runs.list_persisted_tool_runs(kind).await
    }

    async fn list_persisted_tool_runs_for_session(
        &self,
        session_id: &str,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        self.tool_runs
            .list_persisted_tool_runs_for_session(session_id, kind)
            .await
    }

    async fn cancel_for_kind(&self, tool_run_id: &str, kind: &str) -> bool {
        self.tool_runs.cancel_for_kind(tool_run_id, kind).await
    }

    async fn delete_terminal(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        self.tool_runs.delete_terminal(tool_run_id).await
    }

    async fn clear_terminal_history(&self) -> anyhow::Result<u64> {
        self.tool_runs.clear_terminal_history().await
    }

    #[cfg(test)]
    async fn schedule(&self, spec: ScheduledToolRunSpec) -> anyhow::Result<String> {
        self.test_support.schedule(spec).await
    }
}

#[async_trait]
pub(crate) trait AppSkillExecutionPort: Send + Sync {
    async fn execute(
        &self,
        skill: &Skill,
        params: &Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolResult>;
}

pub(crate) fn app_skill_execution_port_from_facade(
    tools: Arc<ToolsFacade>,
) -> Arc<dyn AppSkillExecutionPort> {
    let services = tools.share_services();
    Arc::new(ToolsFacadeSkillExecutionAdapter {
        skill_execution: Arc::clone(&services.skill_execution),
    })
}

struct ToolsFacadeSkillExecutionAdapter {
    skill_execution: Arc<dyn SkillExecutionPort>,
}

#[async_trait]
impl AppSkillExecutionPort for ToolsFacadeSkillExecutionAdapter {
    async fn execute(
        &self,
        skill: &Skill,
        params: &Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.skill_execution.execute(skill, params, cancel).await
    }
}

pub(crate) trait AppLiveOutputPort: Send + Sync {
    fn set_event_sink(&self, sink: Arc<dyn Fn(String, Value) + Send + Sync>);
}

pub(crate) fn app_live_output_port_from_facade(
    tools: Arc<ToolsFacade>,
) -> Arc<dyn AppLiveOutputPort> {
    let services = tools.share_services();
    Arc::new(ToolsFacadeLiveOutputAdapter {
        live_output: Arc::clone(&services.live_output),
    })
}

struct ToolsFacadeLiveOutputAdapter {
    live_output: Arc<dyn LiveOutputSinkPort>,
}

impl AppLiveOutputPort for ToolsFacadeLiveOutputAdapter {
    fn set_event_sink(&self, sink: Arc<dyn Fn(String, Value) + Send + Sync>) {
        self.live_output.set_event_sink(sink);
    }
}

struct ToolsFacadeAgentAdapter {
    tools: Arc<ToolsFacade>,
    authorization: Arc<dyn AuthorizationPort>,
    tool_runs: Arc<dyn ToolRunAgentCapability>,
}

#[async_trait]
impl PromptToolPort for ToolsFacadeAgentAdapter {
    fn catalog_versions(&self) -> PromptCatalogVersions {
        let services = self.tools.share_services();
        PromptCatalogVersions {
            global_catalog_version: self.tools.catalog_version(),
            mcp_catalog_version: self.tools.mcp_catalog_version(),
            skills_catalog_version: services.skills.catalog_version(),
        }
    }

    async fn catalog_content(&self) -> PromptCatalogContent {
        let builtin_tool_definitions = self.tools.list_enabled_builtin_tool_definitions().await;
        PromptCatalogContent {
            builtin_tool_definitions,
            mcp_index: self.tools.build_mcp_index().await,
            skills: self.tools.share_services().skills.list_skill_infos().await,
        }
    }

    async fn runtime_context(&self) -> PromptRuntimeContext {
        let default_shell = self.tools.default_shell_name().await;
        let capabilities = self.tools.runtime_capabilities().await;
        let permission_summary = self.authorization.prompt_summary().await;
        PromptRuntimeContext {
            default_shell,
            capabilities,
            permission_summary,
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
    ) -> RiskLevel {
        self.tools
            .resolve_risk_level(session_id, tool_name, input)
            .await
    }

    async fn authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        self.tools
            .resolve_authorization_request(session_id, tool_name, input)
            .await
    }

    fn authorization_request_from_catalog(
        &self,
        catalog: &ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        self.tools
            .resolve_authorization_request_from_snapshot(catalog, session_id, tool_name, input)
    }

    async fn authorize(&self, request: &AuthorizationRequest) -> AuthorizationDecision {
        self.authorization.authorize(request).await
    }

    async fn verify_receipt(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<(), String> {
        self.authorization.verify_receipt(request, receipt).await
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
        self.authorization.apply_security(security).await;
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

    async fn claim_scheduled_execution(
        &self,
        tool_run_id: &str,
        claim_id: &str,
    ) -> anyhow::Result<bool> {
        self.tool_runs
            .claim_scheduled_execution(tool_run_id, claim_id)
            .await
    }

    async fn release_scheduled_execution_claim(
        &self,
        tool_run_id: &str,
        claim_id: &str,
    ) -> anyhow::Result<bool> {
        self.tool_runs
            .release_scheduled_execution_claim(tool_run_id, claim_id)
            .await
    }

    async fn cancel_owned_by_session_checked(&self, session_id: &str) -> anyhow::Result<()> {
        self.tool_runs
            .cancel_owned_by_session_checked(session_id)
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

    async fn complete_scheduled(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        self.tool_runs.complete_scheduled(tool_run_id).await
    }

    async fn complete_scheduled_with_result(
        &self,
        tool_run_id: &str,
        result: &str,
    ) -> anyhow::Result<bool> {
        self.tool_runs
            .complete_scheduled_with_result(tool_run_id, result)
            .await
    }

    async fn fail_scheduled(&self, tool_run_id: &str, reason: &str) -> anyhow::Result<bool> {
        self.tool_runs.fail_scheduled(tool_run_id, reason).await
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

struct ToolsFacadeToolRunCompletionAdapter {
    receiver: Box<dyn ToolRunCompletionCapability>,
}

#[async_trait]
impl ToolRunCompletionReceiverPort for ToolsFacadeToolRunCompletionAdapter {
    async fn recv_scheduled_with_recovery(&mut self) -> Option<ToolRunCompletion> {
        self.receiver.recv_scheduled_with_recovery().await
    }

    async fn recv_result_with_recovery(&mut self) -> Option<ToolRunCompletion> {
        self.receiver.recv_result_with_recovery().await
    }
}

#[async_trait]
impl ToolExecutionPort for ToolsFacadeAgentAdapter {
    async fn execute(&self, context: ToolExecutionContext) -> anyhow::Result<ToolResult> {
        self.tools
            .execute_tool_with_step(
                context.session_id.as_deref(),
                &context.tool_name,
                context.input,
                context.cancel,
                context.step_id.as_deref(),
            )
            .await
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
    use haven_agent::{SessionStore, SessionSupervisor};
    use haven_common::config::ToolConfig;
    use haven_common::types::RiskLevel;
    use haven_memory::Database;
    use haven_tools::{Tool, ToolHandle};
    use serde_json::json;
    use std::collections::HashMap;
    use tokio_util::sync::CancellationToken;

    struct ExecutionContextProbe;

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
        tools.register_for_session(session_id, tool).await;
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
    async fn composition_adapter_forwards_cancellation() {
        let tools = Arc::new(ToolsFacade::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tool: ToolHandle = Arc::new(ExecutionContextProbe);
        tools.register_for_session(session_id, tool).await;
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
            haven_tools::ToolExecutionOutcome::Cancelled
        );
    }

    #[tokio::test]
    async fn prompt_adapter_uses_only_the_published_builtin_catalog() {
        let tools = Arc::new(ToolsFacade::new());
        let tool: ToolHandle = Arc::new(ExecutionContextProbe);
        tools.registry().register(tool).await.unwrap();
        assert!(
            tools
                .registry()
                .get("execution.context_probe")
                .await
                .is_some()
        );
        let services = tools.share_services();
        let adapter = ToolsFacadeAgentAdapter {
            tools: Arc::clone(&tools),
            authorization: Arc::clone(&services.authorization),
            tool_runs: Arc::clone(&services.tool_run_agent),
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
        assert_ne!(
            adapter.catalog_versions().global_catalog_version,
            tools.registry().version()
        );
    }

    #[tokio::test]
    async fn catalog_and_observation_adapters_preserve_session_and_output_contracts() {
        let tools = Arc::new(ToolsFacade::new());
        let services = tools.share_services();
        let adapter = ToolsFacadeAgentAdapter {
            tools: Arc::clone(&tools),
            authorization: Arc::clone(&services.authorization),
            tool_runs: Arc::clone(&services.tool_run_agent),
        };
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tool: ToolHandle = Arc::new(ExecutionContextProbe);
        tools.register_for_session(session_id, tool).await;

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
