//! Agent-owned ports for tool observations.

use async_trait::async_trait;
use haven_common::types::{MessageAttachment, RiskLevel};
use haven_tools::{
    ActionService, AuthorizationEngine, AuthorizationRequest, ToolRegistration, ToolResult,
    ToolsManager,
};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One execution request crossing the Agent-to-Tools runtime boundary.
#[derive(Clone)]
pub(crate) struct ToolExecutionContext {
    pub(crate) session_id: Option<String>,
    pub(crate) tool_name: String,
    pub(crate) input: Value,
    pub(crate) cancel: CancellationToken,
    pub(crate) step_id: Option<String>,
}

/// Minimal live capability needed by the session tool runner.
#[async_trait]
pub(crate) trait ToolExecutionPort: Send + Sync {
    async fn risk_level(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> RiskLevel;

    async fn authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest;

    fn authorization_request_from_catalog(
        &self,
        catalog: &haven_tools::ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest;

    async fn execute(&self, context: ToolExecutionContext) -> anyhow::Result<ToolResult>;

    async fn registrations(
        &self,
        session_id: &str,
        tool_name: &str,
        output: &Value,
    ) -> Vec<ToolRegistration>;
}

/// Production adapter keeps mutable tool lookup and execution in Tools.
pub(crate) struct ToolsManagerToolExecutionAdapter {
    tools: Arc<ToolsManager>,
}

impl ToolsManagerToolExecutionAdapter {
    pub(crate) fn new(tools: Arc<ToolsManager>) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl ToolExecutionPort for ToolsManagerToolExecutionAdapter {
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
        catalog: &haven_tools::ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        self.tools
            .get_authorization_request_from_snapshot(catalog, session_id, tool_name, input)
    }

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

/// Explicit capabilities needed by one session supervisor.
#[derive(Clone)]
pub struct SessionToolPorts {
    pub(super) execution: Arc<dyn ToolExecutionPort>,
    pub(super) catalog: Arc<dyn crate::react::ToolCatalogPort>,
    pub(super) authorization: Arc<AuthorizationEngine>,
    pub(super) actions: Arc<ActionService>,
    pub(super) session_tool_overlay: Arc<dyn SessionToolOverlayPort>,
    pub(super) managed_asset_leases: Arc<dyn ManagedAssetLeasePort>,
    pub(super) observations: Arc<dyn ToolObservationPort>,
}

impl SessionToolPorts {
    pub(crate) fn from_tools_manager(tools: Arc<ToolsManager>) -> Self {
        let services = tools.share_services();
        Self {
            execution: Arc::new(ToolsManagerToolExecutionAdapter::new(Arc::clone(&tools))),
            catalog: Arc::new(crate::react::ToolsManagerToolCatalogAdapter::new(
                Arc::clone(&tools),
            )),
            authorization: services.authorization,
            actions: services.actions,
            session_tool_overlay: Arc::new(ToolsManagerSessionToolOverlayAdapter::new(Arc::clone(
                &tools,
            ))),
            managed_asset_leases: Arc::new(ToolsManagerManagedAssetLeaseAdapter::new(Arc::clone(
                &tools,
            ))),
            observations: Arc::new(ToolsManagerToolObservationAdapter::new(tools)),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_session_tool_overlay(
        mut self,
        session_tool_overlay: Arc<dyn SessionToolOverlayPort>,
    ) -> Self {
        self.session_tool_overlay = session_tool_overlay;
        self
    }
}

/// Formats the bounded observation text for a completed tool result.
#[async_trait]
pub(super) trait ToolObservationPort: Send + Sync {
    async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String;
}

/// Production adapter delegating observation formatting to the shared manager.
pub(super) struct ToolsManagerToolObservationAdapter {
    tools: Arc<ToolsManager>,
}

impl ToolsManagerToolObservationAdapter {
    pub(super) fn new(tools: Arc<ToolsManager>) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl ToolObservationPort for ToolsManagerToolObservationAdapter {
    async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.tools.observation_text(tool_name, result).await
    }
}

/// Agent-owned boundary for registering and releasing session asset leases.
pub(super) trait ManagedAssetLeasePort: Send + Sync {
    fn register_for_session(&self, session_id: &str, attachments: &[MessageAttachment]);
    fn release_for_session(&self, session_id: &str);
}

/// Adapter that keeps managed-asset path validation and registry ownership in
/// `ToolsManager` while exposing only session lease operations to the agent.
pub(super) struct ToolsManagerManagedAssetLeaseAdapter {
    tools: Arc<ToolsManager>,
}

impl ToolsManagerManagedAssetLeaseAdapter {
    pub(super) fn new(tools: Arc<ToolsManager>) -> Self {
        Self { tools }
    }
}

impl ManagedAssetLeasePort for ToolsManagerManagedAssetLeaseAdapter {
    fn register_for_session(&self, session_id: &str, attachments: &[MessageAttachment]) {
        self.tools
            .register_managed_assets_for_session(session_id, attachments);
    }

    fn release_for_session(&self, session_id: &str) {
        self.tools.release_managed_assets_for_session(session_id);
    }
}

/// Agent-owned boundary for restoring and clearing a session's deferred tool
/// overlay. Live tool loading remains on the existing tool execution path.
#[async_trait]
pub(crate) trait SessionToolOverlayPort: Send + Sync {
    async fn unregister_session(&self, session_id: &str);

    async fn register_mcp_for_session(
        &self,
        session_id: &str,
        server_name: &str,
        tool_names: Option<&[String]>,
    ) -> bool;

    async fn load_skill_for_session(&self, session_id: &str, names: Vec<String>) -> bool;

    async fn load_builtin_operations_for_session(
        &self,
        session_id: &str,
        operations: Option<Vec<String>>,
        roots: Option<Vec<String>>,
    ) -> bool;
}

/// Production adapter keeps all tool catalog mutations in `ToolsManager`.
pub(crate) struct ToolsManagerSessionToolOverlayAdapter {
    tools: Arc<ToolsManager>,
}

impl ToolsManagerSessionToolOverlayAdapter {
    pub(crate) fn new(tools: Arc<ToolsManager>) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl SessionToolOverlayPort for ToolsManagerSessionToolOverlayAdapter {
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
    use haven_common::config::ToolConfig;
    use haven_common::types::RiskLevel;
    use haven_tools::{Tool, ToolBox};
    use serde_json::json;
    use std::collections::HashMap;
    use std::fs;
    use tokio_util::sync::CancellationToken;

    struct OverlayProbeTool(&'static str);

    #[async_trait]
    impl Tool for OverlayProbeTool {
        fn name(&self) -> String {
            self.0.to_string()
        }

        fn description(&self) -> String {
            "session overlay test tool".into()
        }

        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }

        fn input_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(json!({})))
        }
    }

    async fn register_overlay_tool(tools: &ToolsManager, session_id: &str) {
        let tool: ToolBox = Arc::new(OverlayProbeTool("overlay.probe"));
        tools.register_for_session(session_id, tool).await;
    }

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
            json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "additionalProperties": false
            })
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
    async fn execution_adapter_forwards_cancel_and_trusted_step_identity() {
        let tools = Arc::new(ToolsManager::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        tools
            .register_for_session(session_id, Arc::new(ExecutionContextProbe))
            .await;
        let adapter = ToolsManagerToolExecutionAdapter::new(tools);
        let cancel = CancellationToken::new();

        let result = adapter
            .execute(ToolExecutionContext {
                session_id: Some(session_id.into()),
                tool_name: "execution.context_probe".into(),
                input: json!({
                    "value": "kept",
                    "_session_id": "ses-forged",
                    "_step_id": "step-forged"
                }),
                cancel,
                step_id: Some("step-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            })
            .await
            .unwrap();

        assert_eq!(
            result.output,
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
    async fn execution_adapter_forwards_cancellation() {
        let tools = Arc::new(ToolsManager::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        tools
            .register_for_session(session_id, Arc::new(ExecutionContextProbe))
            .await;
        let adapter = ToolsManagerToolExecutionAdapter::new(tools);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = adapter
            .execute(ToolExecutionContext {
                session_id: Some(session_id.into()),
                tool_name: "execution.context_probe".into(),
                input: json!({"value": "kept"}),
                cancel,
                step_id: None,
            })
            .await
            .unwrap();

        assert_eq!(result.outcome, haven_tools::ToolExecutionOutcome::Cancelled);
    }

    #[tokio::test]
    async fn manager_adapter_forwards_tool_name_and_result_to_formatter() {
        let tools = Arc::new(ToolsManager::new());
        let mut settings = HashMap::new();
        settings.insert(
            "probe.operation".into(),
            ToolConfig {
                max_output_chars: Some(4),
                ..ToolConfig::default()
            },
        );
        tools.set_tool_settings(settings).await.unwrap();
        let result = ToolResult::ok(json!("012345"));
        let adapter = ToolsManagerToolObservationAdapter::new(Arc::clone(&tools));

        assert_eq!(
            adapter.observation_text("probe.operation", &result).await,
            "0123"
        );
    }

    #[tokio::test]
    async fn manager_overlay_adapter_loads_and_unregisters_only_the_target_session() {
        let tools = Arc::new(ToolsManager::new());
        let skills_root = tempfile::tempdir().unwrap();
        let skill_dir = skills_root.path().join("echo");
        fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "# Skill: echo\n## Metadata\n- description: echo skill\n## Instructions\ndo echo\n",
        )
        .unwrap();
        tools
            .share_services()
            .skills
            .set_config(Some(skills_root.path().to_path_buf()), None)
            .await
            .unwrap();
        tools.rebuild_catalog().await.unwrap();
        let adapter = ToolsManagerSessionToolOverlayAdapter::new(Arc::clone(&tools));
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let other_session_id = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        assert!(
            adapter
                .load_builtin_operations_for_session(
                    session_id,
                    Some(vec!["files.list".into()]),
                    None,
                )
                .await
        );
        assert!(
            adapter
                .load_skill_for_session(session_id, vec!["echo".into()])
                .await
        );
        assert!(
            !adapter
                .register_mcp_for_session(session_id, "missing-server", None)
                .await
        );
        assert!(
            tools
                .list_schemas_for_session(session_id)
                .await
                .iter()
                .any(|schema| schema["name"] == "files.list")
        );
        assert!(
            tools
                .list_schemas_for_session(session_id)
                .await
                .iter()
                .any(|schema| schema["name"] == "skill__echo")
        );
        assert!(
            !tools
                .list_schemas_for_session(other_session_id)
                .await
                .iter()
                .any(|schema| schema["name"] == "skill__echo")
        );

        adapter.unregister_session(session_id).await;
        assert!(
            !tools
                .list_schemas_for_session(session_id)
                .await
                .iter()
                .any(|schema| {
                    schema["name"] == "files.list" || schema["name"] == "skill__echo"
                })
        );
        assert!(
            !tools
                .list_schemas_for_session(other_session_id)
                .await
                .iter()
                .any(|schema| schema["name"] == "skill__echo")
        );
    }

    #[tokio::test]
    async fn lifecycle_overlay_cleanup_preserves_pause_and_other_sessions() {
        let db_dir = tempfile::tempdir().unwrap();
        let db =
            Arc::new(haven_memory::Database::open(&db_dir.path().join("lifecycle.db")).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let supervisor = crate::session::SessionSupervisor::new_for_test(db, tools.clone(), 1);
        let ended = supervisor.create_session("ended overlay").await.unwrap();
        let removed = supervisor.create_session("removed overlay").await.unwrap();
        let neighbor = supervisor.create_session("neighbor overlay").await.unwrap();

        for session_id in [&ended.id, &removed.id, &neighbor.id] {
            register_overlay_tool(&tools, session_id).await;
        }

        supervisor
            .update_session_status(&ended.id, haven_common::lifecycle::SessionStatus::Paused)
            .await
            .unwrap();
        assert!(
            tools
                .list_schemas_for_session(&ended.id)
                .await
                .iter()
                .any(|schema| schema["name"] == "overlay.probe"),
            "pausing must leave the per-session overlay registered"
        );

        supervisor.end_session(&ended.id).await.unwrap();
        assert!(
            !tools
                .list_schemas_for_session(&ended.id)
                .await
                .iter()
                .any(|schema| schema["name"] == "overlay.probe"),
            "terminal cleanup must unregister the ended session overlay"
        );

        supervisor
            .update_session_status(&removed.id, haven_common::lifecycle::SessionStatus::Paused)
            .await
            .unwrap();
        supervisor.remove_session(&removed.id).await.unwrap();
        assert!(
            !tools
                .list_schemas_for_session(&removed.id)
                .await
                .iter()
                .any(|schema| schema["name"] == "overlay.probe"),
            "removal must unregister the removed session overlay"
        );
        assert!(
            tools
                .list_schemas_for_session(&neighbor.id)
                .await
                .iter()
                .any(|schema| schema["name"] == "overlay.probe"),
            "ending or removing one session must not affect another session"
        );
    }

    #[test]
    fn managed_asset_adapter_releases_only_the_requested_session_lease() {
        let tools = Arc::new(ToolsManager::new());
        let assets = tools.share_services().assets;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("asset.png");
        fs::write(&path, b"asset").unwrap();

        assert!(assets.register_under_root_for_session(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            directory.path(),
            "asset-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            path,
            Some("asset.png".into()),
            "image/png",
        ));
        assert!(assets.lease_for_session(
            "ses-cccccccccccccccccccccccccccccccc",
            "asset-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        ));

        let adapter = ToolsManagerManagedAssetLeaseAdapter::new(tools);
        adapter.release_for_session("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        assert_eq!(
            assets.release_session("ses-cccccccccccccccccccccccccccccccc"),
            1,
            "releasing one session must leave another session's lease intact"
        );
        assert_eq!(
            assets.release_session("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            0,
            "the adapter must release the requested session lease"
        );
    }
}
