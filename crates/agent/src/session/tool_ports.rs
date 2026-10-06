//! Agent-owned ports for tool observations.

use async_trait::async_trait;
use haven_common::types::{MessageAttachment, RiskLevel};
#[cfg(test)]
use haven_tools::ToolsFacade;
use haven_tools::{
    AuthorizationEngine, AuthorizationRequest, ToolRegistration, ToolResult, ToolRunService,
};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One execution request crossing the Agent-to-Tools runtime boundary.
#[derive(Clone)]
pub struct ToolExecutionContext {
    /// Trusted session identity. It is never read from model-supplied input.
    pub session_id: Option<String>,
    pub tool_name: String,
    pub input: Value,
    /// Cancellation authority for the owning run or detached ToolRun.
    pub cancel: CancellationToken,
    /// Stable step identity, separate from the provider's input payload.
    pub step_id: Option<String>,
}

/// Executes an already-authorized tool request and resolves its registrations.
#[async_trait]
pub trait ToolExecutionPort: Send + Sync {
    async fn execute(&self, context: ToolExecutionContext) -> anyhow::Result<ToolResult>;

    async fn registrations(
        &self,
        session_id: &str,
        tool_name: &str,
        output: &Value,
    ) -> Vec<ToolRegistration>;
}

/// Prepares authorization requests from live policy or a turn's catalog view.
/// Decisions and confirmation receipts remain owned by `AuthorizationEngine`.
#[async_trait]
pub trait ToolAuthorizationPort: Send + Sync {
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
}

/// Test adapter mirrors the app composition adapter's execution calls.
#[cfg(test)]
pub(crate) struct ToolsFacadeToolExecutionAdapter {
    tools: Arc<ToolsFacade>,
}

#[cfg(test)]
impl ToolsFacadeToolExecutionAdapter {
    pub(crate) fn new(tools: Arc<ToolsFacade>) -> Self {
        Self { tools }
    }
}

#[cfg(test)]
#[async_trait]
impl ToolExecutionPort for ToolsFacadeToolExecutionAdapter {
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

/// Test adapter for live authorization request preparation.
#[cfg(test)]
pub(crate) struct ToolsFacadeToolAuthorizationAdapter {
    tools: Arc<ToolsFacade>,
}

#[cfg(test)]
impl ToolsFacadeToolAuthorizationAdapter {
    pub(crate) fn new(tools: Arc<ToolsFacade>) -> Self {
        Self { tools }
    }
}

#[cfg(test)]
#[async_trait]
impl ToolAuthorizationPort for ToolsFacadeToolAuthorizationAdapter {
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
        catalog: &haven_tools::ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        self.tools
            .resolve_authorization_request_from_snapshot(catalog, session_id, tool_name, input)
    }
}

/// Explicit capabilities needed by one session supervisor.
#[derive(Clone)]
pub struct SessionToolPorts {
    pub(super) execution: Arc<dyn ToolExecutionPort>,
    pub(super) tool_authorization: Arc<dyn ToolAuthorizationPort>,
    pub(super) catalog: Arc<dyn crate::ToolCatalogPort>,
    pub(super) authorization: Arc<AuthorizationEngine>,
    pub(super) tool_runs: Arc<ToolRunService>,
    pub(super) session_tool_overlay: Arc<dyn SessionToolOverlayPort>,
    pub(super) managed_asset_leases: Arc<dyn ManagedAssetLeasePort>,
    pub(super) observations: Arc<dyn ToolObservationPort>,
}

impl SessionToolPorts {
    /// Build one session's runtime capabilities from explicit typed ports.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        execution: Arc<dyn ToolExecutionPort>,
        tool_authorization: Arc<dyn ToolAuthorizationPort>,
        catalog: Arc<dyn crate::ToolCatalogPort>,
        authorization: Arc<AuthorizationEngine>,
        tool_runs: Arc<ToolRunService>,
        session_tool_overlay: Arc<dyn SessionToolOverlayPort>,
        managed_asset_leases: Arc<dyn ManagedAssetLeasePort>,
        observations: Arc<dyn ToolObservationPort>,
    ) -> Self {
        Self {
            execution,
            tool_authorization,
            catalog,
            authorization,
            tool_runs,
            session_tool_overlay,
            managed_asset_leases,
            observations,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_tools_facade(tools: Arc<ToolsFacade>) -> Self {
        let services = tools.share_services();
        let catalog: Arc<dyn crate::ToolCatalogPort> = Arc::new(
            crate::react::ToolsFacadeToolCatalogAdapter::new(Arc::clone(&tools)),
        );
        Self::new(
            Arc::new(ToolsFacadeToolExecutionAdapter::new(Arc::clone(&tools))),
            Arc::new(ToolsFacadeToolAuthorizationAdapter::new(Arc::clone(&tools))),
            catalog,
            services.authorization,
            services.tool_runs,
            Arc::new(ToolsFacadeSessionToolOverlayAdapter::new(Arc::clone(
                &tools,
            ))),
            Arc::new(ToolsFacadeManagedAssetLeaseAdapter::new(Arc::clone(&tools))),
            Arc::new(ToolsFacadeToolObservationAdapter::new(tools)),
        )
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
pub trait ToolObservationPort: Send + Sync {
    async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String;
}

/// Test adapter delegating observation formatting to the shared Tools facade.
#[cfg(test)]
pub(super) struct ToolsFacadeToolObservationAdapter {
    tools: Arc<ToolsFacade>,
}

#[cfg(test)]
impl ToolsFacadeToolObservationAdapter {
    pub(super) fn new(tools: Arc<ToolsFacade>) -> Self {
        Self { tools }
    }
}

#[cfg(test)]
#[async_trait]
impl ToolObservationPort for ToolsFacadeToolObservationAdapter {
    async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.tools.observation_text(tool_name, result).await
    }
}

/// Agent-owned boundary for registering and releasing session asset leases.
pub trait ManagedAssetLeasePort: Send + Sync {
    fn register_for_session(&self, session_id: &str, attachments: &[MessageAttachment]);
    fn release_for_session(&self, session_id: &str);
}

/// Test adapter mirroring the managed-asset session lease boundary.
#[cfg(test)]
pub(super) struct ToolsFacadeManagedAssetLeaseAdapter {
    tools: Arc<ToolsFacade>,
}

#[cfg(test)]
impl ToolsFacadeManagedAssetLeaseAdapter {
    pub(super) fn new(tools: Arc<ToolsFacade>) -> Self {
        Self { tools }
    }
}

#[cfg(test)]
impl ManagedAssetLeasePort for ToolsFacadeManagedAssetLeaseAdapter {
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
pub trait SessionToolOverlayPort: Send + Sync {
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

/// Test adapter mirroring session overlay operations in the composition layer.
#[cfg(test)]
pub(crate) struct ToolsFacadeSessionToolOverlayAdapter {
    tools: Arc<ToolsFacade>,
}

#[cfg(test)]
impl ToolsFacadeSessionToolOverlayAdapter {
    pub(crate) fn new(tools: Arc<ToolsFacade>) -> Self {
        Self { tools }
    }
}

#[cfg(test)]
#[async_trait]
impl SessionToolOverlayPort for ToolsFacadeSessionToolOverlayAdapter {
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
    use haven_tools::{Tool, ToolHandle};
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

    async fn register_overlay_tool(tools: &ToolsFacade, session_id: &str) {
        let tool: ToolHandle = Arc::new(OverlayProbeTool("overlay.probe"));
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
        let tools = Arc::new(ToolsFacade::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        tools
            .register_for_session(session_id, Arc::new(ExecutionContextProbe))
            .await;
        let adapter = ToolsFacadeToolExecutionAdapter::new(tools);
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
        let tools = Arc::new(ToolsFacade::new());
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        tools
            .register_for_session(session_id, Arc::new(ExecutionContextProbe))
            .await;
        let adapter = ToolsFacadeToolExecutionAdapter::new(tools);
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
        let tools = Arc::new(ToolsFacade::new());
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
        let adapter = ToolsFacadeToolObservationAdapter::new(Arc::clone(&tools));

        assert_eq!(
            adapter.observation_text("probe.operation", &result).await,
            "0123"
        );
    }

    #[tokio::test]
    async fn manager_overlay_adapter_loads_and_unregisters_only_the_target_session() {
        let tools = Arc::new(ToolsFacade::new());
        let skills_root = tempfile::tempdir().unwrap();
        let skill_dir = skills_root.path().join("echo");
        fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "# Skill: echo\n## Metadata\n- description: echo skill\n## Instructions\ndo echo\n",
        )
        .unwrap();
        fs::write(skill_dir.join("scripts").join("main.py"), "print('{}')\n").unwrap();
        tools
            .share_services()
            .skills
            .set_config(Some(skills_root.path().to_path_buf()), None)
            .await
            .unwrap();
        tools.rebuild_catalog().await.unwrap();
        let adapter = ToolsFacadeSessionToolOverlayAdapter::new(Arc::clone(&tools));
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
        let tools = Arc::new(ToolsFacade::new());
        let supervisor = Arc::new(crate::session::SessionSupervisor::new_for_test(
            db,
            tools.clone(),
            1,
        ));
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
        let tools = Arc::new(ToolsFacade::new());
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

        let adapter = ToolsFacadeManagedAssetLeaseAdapter::new(tools);
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
