//! ToolsManager adapters owned by the application composition boundary.

use std::sync::Arc;

use async_trait::async_trait;
use haven_agent::{
    AgentToolPorts, ManagedAssetLeasePort, PromptCatalogContent, PromptCatalogVersions,
    PromptRuntimeContext, PromptToolPort, SessionToolOverlayPort, SessionToolPorts,
    ToolAuthorizationPort, ToolCatalogPort, ToolExecutionContext, ToolExecutionPort,
    ToolObservationPort,
};
use haven_common::types::{MessageAttachment, RiskLevel};
use haven_tools::{
    AuthorizationRequest, ToolCatalogSnapshot, ToolRegistration, ToolResult, ToolsManager,
};
use serde_json::Value;

/// Assemble the one shared ToolsManager into the narrow capabilities consumed
/// by Agent runtime owners. The adapter stays in app-binary so haven-agent
/// depends on ports and tool DTOs, never on the manager facade.
pub(crate) fn agent_tool_ports_from_manager(tools: Arc<ToolsManager>) -> AgentToolPorts {
    let adapter = Arc::new(ToolsManagerAgentAdapter {
        tools: Arc::clone(&tools),
    });
    let services = tools.share_services();

    let prompt: Arc<dyn PromptToolPort> = adapter.clone();
    let catalog: Arc<dyn ToolCatalogPort> = adapter.clone();
    let session = SessionToolPorts::new(
        adapter.clone(),
        adapter.clone(),
        Arc::clone(&catalog),
        services.authorization,
        services.actions,
        adapter.clone(),
        adapter.clone(),
        adapter,
    );

    AgentToolPorts::new(prompt, catalog, session)
}

struct ToolsManagerAgentAdapter {
    tools: Arc<ToolsManager>,
}

#[async_trait]
impl PromptToolPort for ToolsManagerAgentAdapter {
    fn catalog_versions(&self) -> PromptCatalogVersions {
        let services = self.tools.share_services();
        PromptCatalogVersions {
            registry: self.tools.registry().version(),
            mcp: self.tools.mcp_catalog_version(),
            skills: services.skills.catalog_version(),
        }
    }

    async fn catalog_content(&self) -> PromptCatalogContent {
        let mut builtin_defs = self.tools.list_enabled_builtin_defs().await;
        // A small embedding may build a prompt before asynchronous builtin
        // catalog initialization has run. Preserve the eager-registry fallback.
        if builtin_defs.is_empty() {
            builtin_defs = self.tools.registry().list_defs().await;
        }
        PromptCatalogContent {
            builtin_defs,
            mcp_index: self.tools.build_mcp_index().await,
            skills: self.tools.share_services().skills.list().await,
        }
    }

    async fn runtime_context(&self) -> PromptRuntimeContext {
        let services = self.tools.share_services();
        let default_shell = self.tools.default_shell_name().await;
        let capabilities = self.tools.runtime_capabilities().await;
        let permission_summary = services.authorization.prompt_summary().await;
        PromptRuntimeContext {
            default_shell,
            capabilities,
            permission_summary,
        }
    }
}

#[async_trait]
impl ToolCatalogPort for ToolsManagerAgentAdapter {
    async fn catalog_snapshot(&self, session_id: &str) -> Arc<ToolCatalogSnapshot> {
        Arc::new(self.tools.tool_catalog_snapshot(session_id).await)
    }
}

#[async_trait]
impl ToolAuthorizationPort for ToolsManagerAgentAdapter {
    async fn risk_level(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> RiskLevel {
        self.tools
            .get_risk_level(session_id, tool_name, input)
            .await
    }

    async fn authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        self.tools
            .get_authorization_request(session_id, tool_name, input)
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
            .get_authorization_request_from_snapshot(catalog, session_id, tool_name, input)
    }
}

#[async_trait]
impl ToolExecutionPort for ToolsManagerAgentAdapter {
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
impl ToolObservationPort for ToolsManagerAgentAdapter {
    async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.tools.observation_text(tool_name, result).await
    }
}

impl ManagedAssetLeasePort for ToolsManagerAgentAdapter {
    fn register_for_session(&self, session_id: &str, attachments: &[MessageAttachment]) {
        self.tools
            .register_managed_assets_for_session(session_id, attachments);
    }

    fn release_for_session(&self, session_id: &str) {
        self.tools.release_managed_assets_for_session(session_id);
    }
}

#[async_trait]
impl SessionToolOverlayPort for ToolsManagerAgentAdapter {
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
    use haven_tools::{Tool, ToolBox};
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
        let tools = Arc::new(ToolsManager::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tool: ToolBox = Arc::new(ExecutionContextProbe);
        tools.register_for_session(session_id, tool).await;
        let ports = agent_tool_ports_from_manager(Arc::clone(&tools));
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
        let tools = Arc::new(ToolsManager::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tool: ToolBox = Arc::new(ExecutionContextProbe);
        tools.register_for_session(session_id, tool).await;
        let ports = agent_tool_ports_from_manager(tools);
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
    async fn prompt_adapter_preserves_eager_builtin_catalog_fallback() {
        let tools = Arc::new(ToolsManager::new());
        let tool: ToolBox = Arc::new(ExecutionContextProbe);
        tools.registry().register(tool).await.unwrap();
        let adapter = ToolsManagerAgentAdapter {
            tools: Arc::clone(&tools),
        };

        let content = adapter.catalog_content().await;

        assert!(
            content
                .builtin_defs
                .iter()
                .any(|definition| definition.name == "execution.context_probe"),
            "prompt construction falls back to definitions already present in the eager registry"
        );
        assert_eq!(
            adapter.catalog_versions().registry,
            tools.registry().version()
        );
    }

    #[tokio::test]
    async fn catalog_and_observation_adapters_preserve_session_and_output_contracts() {
        let tools = Arc::new(ToolsManager::new());
        let adapter = ToolsManagerAgentAdapter {
            tools: Arc::clone(&tools),
        };
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let tool: ToolBox = Arc::new(ExecutionContextProbe);
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
