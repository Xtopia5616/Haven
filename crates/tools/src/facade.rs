use super::*;
use crate::live_output::LiveOutputHub;
#[cfg(test)]
use crate::registry::OperationRegistry;
use crate::skill_runner::SkillRunner;
use futures_util::future::BoxFuture;
use haven_mcp::{
    McpElicitationAnswer, McpFeatureCallOutcome, McpManager, McpPromptList, McpResourceChangeEvent,
    McpResourceList, McpResourceTemplateList,
};
#[cfg(feature = "test-support")]
use haven_skills::Skill;
use haven_skills::SkillRegistry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// A target invocation deferred into the ToolRun-owned background task.
pub type ToolRunInvocation = Box<
    dyn FnOnce(CancellationToken) -> BoxFuture<'static, anyhow::Result<ToolResult>>
        + Send
        + 'static,
>;

/// Capability for resolving and running a named Skill inside the Tools owner.
#[async_trait::async_trait]
pub trait SkillExecutionPort: Send + Sync {
    async fn execute_skill(
        &self,
        skill_name: &str,
        params: &Value,
        request: &crate::AuthorizationRequest,
        proof: &crate::AuthorizationExecutionProof,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolResult>;
}

/// Capability for installing the sink used by live foreground tool previews.
pub trait LiveOutputSinkPort: Send + Sync {
    fn set_event_sink(&self, sink: Arc<dyn Fn(String, Value) + Send + Sync>);
}

/// App-facing MCP runtime queries. Connection management and client state stay
/// owned by `McpManager`; consumers receive only status, reconciliation and
/// event capabilities.
#[async_trait::async_trait]
pub trait McpRuntimePort: Send + Sync {
    async fn start_monitors(&self, config: &haven_common::config::McpDiscoveryConfig);

    async fn shutdown(&self);

    async fn snapshot(&self) -> Vec<McpServerSnapshot>;

    async fn list_resources_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceList>;

    async fn list_resource_templates_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceTemplateList>;

    async fn read_resource_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        uri: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpFeatureCallOutcome>;

    async fn respond_to_feature_input_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        request_id: u64,
        answers: HashMap<String, McpElicitationAnswer>,
    ) -> anyhow::Result<McpFeatureCallOutcome>;

    async fn cancel_feature_input_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        request_id: u64,
    ) -> anyhow::Result<bool>;

    async fn subscribe_resource_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        uri: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<bool>;

    async fn unsubscribe_resource_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        uri: &str,
    ) -> anyhow::Result<bool>;

    async fn list_prompts_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpPromptList>;

    async fn get_prompt_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        name: &str,
        arguments: HashMap<String, String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpFeatureCallOutcome>;

    async fn has_client(&self, name: &str) -> bool;

    async fn client_status(&self, name: &str) -> McpClientStatus;

    async fn plan_refresh(
        &self,
        config_version: haven_common::config::ConfigVersion,
        servers: &[McpServerConfig],
    ) -> McpRefreshPlan;

    fn subscribe_status_events(&self) -> Box<dyn McpStatusEventReceiver>;

    fn subscribe_resource_change_events(&self) -> Box<dyn McpResourceChangeEventReceiver>;
}

/// Cancellable-by-drop receiver for MCP status changes without exposing the
/// manager's broadcast transport.
#[async_trait::async_trait]
pub trait McpStatusEventReceiver: Send {
    async fn recv(&mut self) -> Option<McpStatusChangeEvent>;
}

#[async_trait::async_trait]
pub trait McpResourceChangeEventReceiver: Send {
    async fn recv(&mut self) -> Option<McpResourceChangeEvent>;
}

struct SharedMcpRuntimePort {
    manager: McpManager,
}

#[async_trait::async_trait]
impl McpRuntimePort for SharedMcpRuntimePort {
    async fn start_monitors(&self, config: &haven_common::config::McpDiscoveryConfig) {
        self.manager.start_monitors(config).await;
    }

    async fn shutdown(&self) {
        self.manager.shutdown_all().await;
    }

    async fn snapshot(&self) -> Vec<McpServerSnapshot> {
        self.manager.snapshot().await
    }

    async fn list_resources_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceList> {
        self.manager
            .list_resources_at_target_binding(client_name, target_binding, force_refresh, cancel)
            .await
    }

    async fn list_resource_templates_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceTemplateList> {
        self.manager
            .list_resource_templates_at_target_binding(
                client_name,
                target_binding,
                force_refresh,
                cancel,
            )
            .await
    }

    async fn read_resource_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        uri: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpFeatureCallOutcome> {
        self.manager
            .read_resource_at_target_binding(
                client_name,
                target_binding,
                uri,
                force_refresh,
                cancel,
            )
            .await
    }

    async fn respond_to_feature_input_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        request_id: u64,
        answers: HashMap<String, McpElicitationAnswer>,
    ) -> anyhow::Result<McpFeatureCallOutcome> {
        self.manager
            .respond_to_feature_input_at_target_binding(
                client_name,
                target_binding,
                request_id,
                answers,
            )
            .await
    }

    async fn cancel_feature_input_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        request_id: u64,
    ) -> anyhow::Result<bool> {
        self.manager
            .cancel_feature_input_at_target_binding(client_name, target_binding, request_id)
            .await
    }

    async fn subscribe_resource_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        uri: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<bool> {
        self.manager
            .subscribe_resource_at_target_binding(client_name, target_binding, uri, cancel)
            .await
    }

    async fn unsubscribe_resource_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        uri: &str,
    ) -> anyhow::Result<bool> {
        self.manager
            .unsubscribe_resource_at_target_binding(client_name, target_binding, uri)
            .await
    }

    async fn list_prompts_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpPromptList> {
        self.manager
            .list_prompts_at_target_binding(client_name, target_binding, force_refresh, cancel)
            .await
    }

    async fn get_prompt_at_target_binding(
        &self,
        client_name: &str,
        target_binding: &str,
        name: &str,
        arguments: HashMap<String, String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpFeatureCallOutcome> {
        self.manager
            .get_prompt_at_target_binding(client_name, target_binding, name, arguments, cancel)
            .await
    }

    async fn has_client(&self, name: &str) -> bool {
        self.manager.has_client(name).await
    }

    async fn client_status(&self, name: &str) -> McpClientStatus {
        self.manager.client_status(name).await
    }

    async fn plan_refresh(
        &self,
        config_version: haven_common::config::ConfigVersion,
        servers: &[McpServerConfig],
    ) -> McpRefreshPlan {
        let reconcile = self.manager.reconcile_servers(servers).await;
        McpRefreshPlan::from_reconcile(config_version, &reconcile)
    }

    fn subscribe_status_events(&self) -> Box<dyn McpStatusEventReceiver> {
        Box::new(BroadcastMcpStatusEventReceiver {
            receiver: self.manager.subscribe(),
        })
    }

    fn subscribe_resource_change_events(&self) -> Box<dyn McpResourceChangeEventReceiver> {
        Box::new(BroadcastMcpResourceChangeEventReceiver {
            receiver: self.manager.subscribe_resource_changes(),
        })
    }
}

struct BroadcastMcpStatusEventReceiver {
    receiver: tokio::sync::broadcast::Receiver<McpStatusChangeEvent>,
}

#[async_trait::async_trait]
impl McpStatusEventReceiver for BroadcastMcpStatusEventReceiver {
    async fn recv(&mut self) -> Option<McpStatusChangeEvent> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Some(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

struct BroadcastMcpResourceChangeEventReceiver {
    receiver: tokio::sync::broadcast::Receiver<McpResourceChangeEvent>,
}

#[async_trait::async_trait]
impl McpResourceChangeEventReceiver for BroadcastMcpResourceChangeEventReceiver {
    async fn recv(&mut self) -> Option<McpResourceChangeEvent> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Some(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Narrow Skills catalog and configuration operations consumed outside Tools.
#[async_trait::async_trait]
pub trait SkillCatalogPort: Send + Sync {
    fn catalog_version(&self) -> u64;

    async fn set_config(
        &self,
        root: Option<PathBuf>,
        enabled: Option<Vec<String>>,
    ) -> anyhow::Result<()>;

    async fn refresh_from_disk(&self) -> anyhow::Result<()>;

    async fn list_skill_infos(&self) -> Vec<SkillInfo>;

    async fn get_skill_info(&self, name: &str) -> Option<SkillInfo>;

    async fn resolved_root(&self) -> PathBuf;
}

/// App-facing capability for metadata lookup and execution of one typed admin
/// request. The concrete admin operation set remains owned by Tools.
#[async_trait::async_trait]
pub trait AdminOperationPort: Send + Sync {
    fn risk_level(&self, request: &AdminRequest) -> haven_common::types::RiskLevel;

    async fn execute(
        &self,
        request: AdminRequest,
        cancel: CancellationToken,
    ) -> Result<ToolResult, AdminOperationError>;
}

struct SharedAdminOperationPort {
    operation_set: Arc<crate::builtin::admin::AdminOperationSet>,
}

#[async_trait::async_trait]
impl AdminOperationPort for SharedAdminOperationPort {
    fn risk_level(&self, request: &AdminRequest) -> haven_common::types::RiskLevel {
        self.operation_set.metadata(request).risk_level
    }

    async fn execute(
        &self,
        request: AdminRequest,
        cancel: CancellationToken,
    ) -> Result<ToolResult, AdminOperationError> {
        self.operation_set.execute(request, cancel).await
    }
}

struct SharedSkillCatalogPort {
    registry: SkillRegistry,
}

#[async_trait::async_trait]
impl SkillCatalogPort for SharedSkillCatalogPort {
    fn catalog_version(&self) -> u64 {
        self.registry.catalog_version()
    }

    async fn set_config(
        &self,
        root: Option<PathBuf>,
        enabled: Option<Vec<String>>,
    ) -> anyhow::Result<()> {
        self.registry.set_config(root, enabled).await
    }

    async fn refresh_from_disk(&self) -> anyhow::Result<()> {
        self.registry.refresh_from_disk().await
    }

    async fn list_skill_infos(&self) -> Vec<SkillInfo> {
        self.registry.list_skill_infos().await
    }

    async fn get_skill_info(&self, name: &str) -> Option<SkillInfo> {
        self.registry.get_skill_info(name).await
    }

    async fn resolved_root(&self) -> PathBuf {
        self.registry.resolved_root().await
    }
}

/// Agent-facing operations for detached background ToolRuns.
#[async_trait::async_trait]
pub trait ToolRunAgentCapability: Send + Sync {
    async fn start_background(
        &self,
        tool_name: String,
        source_step_id: Option<String>,
        admission_cancel: CancellationToken,
        invocation: ToolRunInvocation,
    ) -> anyhow::Result<String>;

    async fn attach_session(&self, tool_run_id: &str, session_id: &str);

    async fn cancel_owned_background_by_session_checked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<()>;

    async fn cancel_owned_background_by_session(&self, session_id: &str);

    async fn list_for_session_views(&self, session_id: &str) -> Vec<ToolRunListView>;

    async fn restore(&self) -> ToolRunRestoreSummary;

    async fn acknowledge_completion(&self, tool_run_result_id: &str);

    async fn acknowledge_unowned_completion(&self, tool_run_result_id: &str);

    fn take_completion_receiver(&self) -> Option<Box<dyn ToolRunCompletionCapability>>;
}

/// Completion stream with durable recovery kept inside the Tools owner.
#[async_trait::async_trait]
pub trait ToolRunCompletionCapability: Send {
    async fn recv_result_with_recovery(&mut self) -> Option<ToolRunCompletion>;
}

/// Schedule-facing operations used by Agent's trigger executor. Schedule
/// state and fired delivery remain owned by ScheduleService.
#[async_trait::async_trait]
pub trait ScheduleAgentCapability: Send + Sync {
    async fn restore(&self) -> anyhow::Result<ScheduleRestoreSummary>;

    async fn cancel_owned_by_session_checked(&self, session_id: &str) -> anyhow::Result<()>;

    async fn claim_execution(&self, schedule_run_id: &str, claim_id: &str) -> anyhow::Result<bool>;

    async fn release_execution(
        &self,
        schedule_run_id: &str,
        claim_id: &str,
    ) -> anyhow::Result<bool>;

    async fn finish(
        &self,
        schedule_run_id: &str,
        status: haven_common::lifecycle::ScheduleRunStatus,
        result_summary: Option<&str>,
        error_reason: Option<&str>,
    ) -> anyhow::Result<bool>;

    fn take_fired_receiver(&self) -> Option<Box<dyn ScheduleFiredReceiver>>;

    async fn shutdown(&self);
}

#[async_trait::async_trait]
pub trait ScheduleFiredReceiver: Send {
    async fn recv(&mut self) -> Option<ScheduleRunTriggered>;
}

/// Plan operations consumed by the Schedule builtin and App schedule views.
#[async_trait::async_trait]
pub trait ScheduleManagementCapability: Send + Sync {
    async fn create(
        &self,
        input: haven_memory::SchedulePlanInput,
    ) -> anyhow::Result<haven_memory::SchedulePlanRow>;

    async fn list_for_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<haven_memory::SchedulePlanRow>>;

    async fn list_enabled(&self) -> anyhow::Result<Vec<haven_memory::SchedulePlanRow>>;

    async fn cancel(&self, schedule_id: &str, session_id: &str) -> anyhow::Result<bool>;
}

/// App-facing ToolRun lifecycle and management operations.
#[async_trait::async_trait]
pub trait ToolRunManagementCapability: Send + Sync {
    fn set_lifecycle_event_sink(&self, sink: ToolRunLifecycleEventSink);

    async fn shutdown(&self);

    async fn board(&self) -> Vec<ToolRunView>;

    async fn list_board_history(&self) -> anyhow::Result<Vec<ToolRunView>>;

    async fn list_history(&self, query: ToolRunHistoryQuery) -> anyhow::Result<Vec<ToolRunView>>;

    async fn cancel(&self, tool_run_id: &str) -> bool;

    async fn delete_terminal(&self, tool_run_id: &str) -> anyhow::Result<bool>;

    async fn clear_terminal_history(&self) -> anyhow::Result<u64>;
}

/// Test-only ToolRun controls for injecting persistence and deterministic state.
#[cfg(feature = "test-support")]
#[async_trait::async_trait]
pub trait ToolRunTestSupportPort: Send + Sync {
    async fn set_store(&self, store: Option<haven_memory::ToolRunStore>);

    async fn status_view(&self, tool_run_id: &str) -> ToolRunStatusView;

    async fn cancel(&self, tool_run_id: &str) -> bool;

    async fn delete_terminal(&self, tool_run_id: &str) -> anyhow::Result<bool>;
}

/// Test-only controls for arranging and inspecting tool catalog fixtures.
/// Production consumers receive published catalog snapshots instead.
#[cfg(feature = "test-support")]
#[async_trait::async_trait]
pub trait ToolCatalogTestSupportPort: Send + Sync {
    async fn register_installed_tool(&self, tool: ToolHandle) -> anyhow::Result<()>;

    async fn contains_installed_tool(&self, name: &str) -> bool;

    async fn register_session_tool(&self, session_id: &str, tool: ToolHandle);

    async fn upsert_mcp_server_config(&self, config: McpServerConfig);

    async fn remove_mcp_server_config(&self, name: &str);

    async fn get_skill_fixture(&self, name: &str) -> Option<Skill>;
}

#[cfg(feature = "test-support")]
struct SharedToolCatalogTestSupportPort {
    installed_registry: crate::registry::ToolRegistry,
    session_overlay: crate::registry::SessionToolOverlay,
    mcp_server_config_source: crate::McpServerConfigSource,
    mcp_manager: McpManager,
    skill_registry: SkillRegistry,
}

#[cfg(feature = "test-support")]
#[async_trait::async_trait]
impl ToolCatalogTestSupportPort for SharedToolCatalogTestSupportPort {
    async fn register_installed_tool(&self, tool: ToolHandle) -> anyhow::Result<()> {
        self.installed_registry.register(tool).await
    }

    async fn contains_installed_tool(&self, name: &str) -> bool {
        self.installed_registry.get(name).await.is_some()
    }

    async fn register_session_tool(&self, session_id: &str, tool: ToolHandle) {
        self.session_overlay.register(session_id, tool).await;
    }

    async fn upsert_mcp_server_config(&self, config: McpServerConfig) {
        self.mcp_server_config_source.upsert_fixture(config).await;
        self.mcp_manager.invalidate_catalog();
        self.session_overlay.bump_global_version();
    }

    async fn remove_mcp_server_config(&self, name: &str) {
        self.mcp_server_config_source.remove_fixture(name).await;
        self.mcp_manager.invalidate_catalog();
        self.session_overlay.bump_global_version();
    }

    async fn get_skill_fixture(&self, name: &str) -> Option<Skill> {
        self.skill_registry.get_skill(name).await
    }
}

struct SharedSkillExecutionPort {
    registry: SkillRegistry,
    runner: Arc<RwLock<SkillRunner>>,
    authorization: Arc<crate::security::AuthorizationEngine>,
}

#[async_trait::async_trait]
impl SkillExecutionPort for SharedSkillExecutionPort {
    async fn execute_skill(
        &self,
        skill_name: &str,
        params: &Value,
        request: &crate::AuthorizationRequest,
        proof: &crate::AuthorizationExecutionProof,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let tool_name = crate::skill_tool_name(skill_name);
        let capability = haven_common::types::CapabilityScope::try_new(&tool_name)
            .map_err(anyhow::Error::msg)?;
        let expected_policy =
            crate::OperationPolicy::external(capability, haven_common::types::RiskLevel::High);
        if request.session_id.is_some()
            || request.tool_name != tool_name
            || request.input != *params
            || request.target_binding.is_some()
            || request.policy != expected_policy
        {
            anyhow::bail!("Skill execution proof does not match the requested Skill and input");
        }
        self.authorization
            .verify_execution_proof(request, proof)
            .await
            .map_err(anyhow::Error::msg)?;
        let context = self
            .authorization
            .execution_context_for_proof(request, proof)
            .await
            .map_err(anyhow::Error::msg)?;
        let execution_authorization = context
            .clone_execution_authorization()
            .ok_or_else(|| anyhow::anyhow!("Skill execution authorization is unavailable"))?;
        let skill = self.registry.get_executable_skill(skill_name).await?;
        self.runner
            .read()
            .await
            .execute_with_authorization(&skill, params, cancel, execution_authorization)
            .await
    }
}

struct SharedLiveOutputSinkPort {
    hub: Arc<LiveOutputHub>,
}

impl LiveOutputSinkPort for SharedLiveOutputSinkPort {
    fn set_event_sink(&self, sink: Arc<dyn Fn(String, Value) + Send + Sync>) {
        self.hub.set_live_output_event_sink(sink);
    }
}

/// Process services shared outside the execution facade.
///
/// Process services shared outside the execution facade. MCP status/reconcile
/// and Skills catalog operations are exposed through consumer capabilities;
/// their concrete managers stay inside the Tools composition owner.
#[derive(Clone)]
pub struct ToolServices {
    pub mcp: Arc<dyn McpRuntimePort>,
    pub mcp_tool_caller: Arc<dyn haven_llm::McpToolCaller>,
    pub skills: Arc<dyn SkillCatalogPort>,
    pub skill_execution: Arc<dyn SkillExecutionPort>,
    pub authorization: Arc<dyn AuthorizationPort>,
    pub managed_assets: Arc<dyn ManagedAssetLifecyclePort>,
    pub tool_run_agent: Arc<dyn ToolRunAgentCapability>,
    pub tool_run_management: Arc<dyn ToolRunManagementCapability>,
    pub schedule_agent: Arc<dyn ScheduleAgentCapability>,
    pub schedule_management: Arc<dyn ScheduleManagementCapability>,
    #[cfg(feature = "test-support")]
    pub tool_run_test_support: Arc<dyn ToolRunTestSupportPort>,
    #[cfg(feature = "test-support")]
    pub tool_catalog_test_support: Arc<dyn ToolCatalogTestSupportPort>,
    pub live_output: Arc<dyn LiveOutputSinkPort>,
}

impl ToolServices {
    fn from_parts(coordinator: &coordinator::ToolRuntimeCoordinator) -> Self {
        let authorization: Arc<dyn AuthorizationPort> = coordinator.core.authorization.clone();
        let skill_runner = Arc::clone(&coordinator.builtins.skill_runner);
        let live_outputs = Arc::clone(&coordinator.runtime.live_outputs);
        let tool_run_service = Arc::clone(&coordinator.runtime.tool_run_service);
        let schedule_service = Arc::clone(&coordinator.runtime.schedule_service);
        let mcp_manager = coordinator.builtins.mcp_manager.clone();
        Self {
            mcp: Arc::new(SharedMcpRuntimePort {
                manager: mcp_manager.clone(),
            }),
            mcp_tool_caller: crate::mcp_llm_adapter::adapt(mcp_manager.clone()),
            skills: Arc::new(SharedSkillCatalogPort {
                registry: coordinator.builtins.skill_registry.clone(),
            }),
            skill_execution: Arc::new(SharedSkillExecutionPort {
                registry: coordinator.builtins.skill_registry.clone(),
                runner: Arc::clone(&skill_runner),
                authorization: Arc::clone(&coordinator.core.authorization),
            }),
            authorization,
            managed_assets: Arc::new(coordinator.runtime.managed_assets.clone()),
            tool_run_agent: crate::tool_run_capabilities::agent_port(Arc::clone(&tool_run_service)),
            tool_run_management: crate::tool_run_capabilities::management_port(Arc::clone(
                &tool_run_service,
            )),
            schedule_agent: crate::schedule_capabilities::agent_port(Arc::clone(&schedule_service)),
            schedule_management: crate::schedule_capabilities::management_port(schedule_service),
            #[cfg(feature = "test-support")]
            tool_run_test_support: crate::tool_run_capabilities::test_support_port(Arc::clone(
                &tool_run_service,
            )),
            #[cfg(feature = "test-support")]
            tool_catalog_test_support: Arc::new(SharedToolCatalogTestSupportPort {
                installed_registry: coordinator.core.operations.installed.clone(),
                session_overlay: coordinator.core.operations.session_tool_overlay.clone(),
                mcp_server_config_source: coordinator.builtins.mcp_server_config_source.clone(),
                mcp_manager: coordinator.builtins.mcp_manager.clone(),
                skill_registry: coordinator.builtins.skill_registry.clone(),
            }),
            live_output: Arc::new(SharedLiveOutputSinkPort {
                hub: Arc::clone(&live_outputs),
            }),
        }
    }
}

/// Facade for tool execution.
///
/// Callers enter through execution and catalog projection methods. Process
/// services are handed out once via [`ToolServices`]; the facade does not
/// bind individual model or media clients. Those inputs live on one
/// `PlatformRuntime` snapshot owned by `ToolRuntime`. Installed, deferred and
/// session operations live on [`OperationRegistry`].
pub struct ToolsFacade {
    pub(crate) coordinator: coordinator::ToolRuntimeCoordinator,
    services: ToolServices,
}

impl ToolsFacade {
    pub fn new() -> Self {
        Self::new_with_exec_config(SkillsExecConfig::default())
    }

    pub fn new_with_exec_config(exec_config: SkillsExecConfig) -> Self {
        let coordinator = coordinator::ToolRuntimeCoordinator::new(exec_config);
        let services = ToolServices::from_parts(&coordinator);
        Self {
            coordinator,
            services,
        }
    }

    /// Clone of the process-service bundle captured at construction.
    pub fn share_services(&self) -> ToolServices {
        self.services.clone()
    }

    /// Create a non-owning, typed admin capability for live tool toggles.
    /// `Weak` prevents the native admin operation set from forming a facade cycle.
    pub fn tool_control_port(self: &Arc<Self>) -> Arc<dyn ToolControlPort> {
        Arc::new(ToolControlHandle(Arc::downgrade(self)))
    }

    /// Test-only white-box access for assertions owned by the Tools crate.
    #[cfg(test)]
    pub(crate) fn operations(&self) -> &OperationRegistry {
        &self.coordinator.core.operations
    }

    #[cfg(test)]
    pub(crate) fn registry(&self) -> &ToolRegistry {
        self.operations().installed()
    }

    /// Register attachments and hold them for the lifetime of a live session.
    /// This protects event-backed assets before their `messages` projection is
    /// visible to retention cleanup.
    pub fn register_managed_assets_for_session(
        &self,
        session_id: &str,
        attachments: &[MessageAttachment],
    ) {
        let uploads_root = haven_common::config::default_uploads_root();
        let generated_root = haven_common::config::default_generated_media_root();
        for attachment in attachments {
            let (Some(asset_id), Some(path)) = (&attachment.asset_id, &attachment.path) else {
                continue;
            };
            let path = std::path::PathBuf::from(path);
            if self
                .coordinator
                .runtime
                .managed_assets
                .register_under_root_for_session(
                    session_id,
                    &uploads_root,
                    asset_id.clone(),
                    path.clone(),
                    attachment.filename.clone(),
                    attachment.media_type.clone(),
                )
            {
                continue;
            }
            if !self
                .coordinator
                .runtime
                .managed_assets
                .register_under_root_for_session_with_metadata(
                    session_id,
                    &generated_root,
                    asset_id.clone(),
                    path,
                    attachment.filename.clone(),
                    attachment.media_type.clone(),
                    attachment.sha256.clone(),
                    attachment.size_bytes,
                    // Persisted attachment expiry is superseded by session
                    // ownership. Runtime-only generated assets retain their
                    // explicit TTL through the non-session registration API.
                    None,
                )
            {
                tracing::warn!(
                    asset_id = %asset_id,
                    session_id = %session_id,
                    "rejecting managed attachment outside the host uploads root or session lease"
                );
            }
        }
    }

    /// Bind assets registered before new-session allocation to the resulting
    /// session lease.
    pub fn bind_pending_managed_assets_to_session(
        &self,
        session_id: &str,
        attachments: &[MessageAttachment],
    ) {
        for attachment in attachments {
            let Some(asset_id) = attachment.asset_id.as_deref() else {
                continue;
            };
            if !self
                .coordinator
                .runtime
                .managed_assets
                .bind_pending_to_session(session_id, asset_id)
            {
                tracing::warn!(
                    asset_id = %asset_id,
                    session_id = %session_id,
                    "failed to bind pending managed attachment to session lease"
                );
            }
        }
    }

    /// Reassign only the uploaded assets from a stale ingress session id to
    /// the actual session returned by the agent. Other assets already leased
    /// by the stale session remain protected.
    pub fn transfer_managed_assets_to_session(
        &self,
        from_session_id: &str,
        to_session_id: &str,
        attachments: &[MessageAttachment],
    ) {
        for attachment in attachments {
            let Some(asset_id) = attachment.asset_id.as_deref() else {
                continue;
            };
            if !self
                .coordinator
                .runtime
                .managed_assets
                .transfer_session_lease(from_session_id, to_session_id, asset_id)
            {
                tracing::warn!(
                    asset_id = %asset_id,
                    from_session_id = %from_session_id,
                    to_session_id = %to_session_id,
                    "failed to transfer managed attachment session lease"
                );
            }
        }
    }

    /// Release assets registered for an ingress request whose new session was
    /// never created. Unreferenced entries are removed by the next GC pass.
    pub fn release_pending_managed_assets(&self, attachments: &[MessageAttachment]) {
        for attachment in attachments {
            if let Some(asset_id) = attachment.asset_id.as_deref() {
                self.coordinator
                    .runtime
                    .managed_assets
                    .release_pending(asset_id);
            }
        }
    }

    /// Release the process-local asset lease held by a terminal session.
    pub fn release_managed_assets_for_session(&self, session_id: &str) {
        self.coordinator
            .runtime
            .managed_assets
            .release_session(session_id);
    }

    /// Monotonic catalog version (see `catalog_version`). Consumers cache
    /// derived views (e.g. per-step LLM tool definitions) keyed by this
    /// value and rebuild only when it changes.
    pub fn catalog_version(&self) -> u64 {
        self.coordinator
            .core
            .operations
            .session_tool_overlay
            .global_version()
    }

    /// MCP has its own tools/list change clock and therefore must participate
    /// in prompt-index cache keys independently of the global tool catalog.
    pub fn mcp_catalog_version(&self) -> u64 {
        self.coordinator.builtins.mcp_manager.catalog_version()
    }

    /// Version identity for a session's complete tool-definition view.
    pub(crate) async fn catalog_version_for_session(
        &self,
        session_id: &str,
    ) -> crate::ToolCatalogVersion {
        let mut version = self
            .coordinator
            .core
            .operations
            .session_tool_overlay
            .catalog_version_for_session(session_id)
            .await;
        version.mcp_catalog_version = self.coordinator.builtins.mcp_manager.catalog_version();
        version
    }

    /// Capture the complete lookup surface used by one ReAct tool batch.
    ///
    /// Global and session-overlay registries are copied into one name index;
    /// later admission metadata reads are synchronous map lookups rather than
    /// one async catalog walk per call. A bounded version check avoids
    /// publishing a mixed view when a loader updates the session while the
    /// snapshot is being assembled. The final attempt is deliberately used
    /// under sustained catalog churn: this is a performance snapshot, while
    /// the execution boundary remains responsible for a final runtime check.
    pub async fn tool_catalog_snapshot(&self, session_id: &str) -> ToolCatalogSnapshot {
        self.operation_catalog()
            .tool_catalog_snapshot(session_id)
            .await
    }

    /// Rebuild the catalog views derived from the already published shared
    /// model/media generation.
    pub async fn rebuild_model_media_catalog(
        &self,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.coordinator.rebuild_model_media_catalog().await
    }

    #[cfg(test)]
    async fn set_router_and_media_clients(
        &self,
        router: Arc<LlmRouter>,
        stt_client: Option<Arc<dyn haven_llm::SttClient>>,
        ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
        image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
        tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
        media_config: haven_common::config::MediaConfig,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.coordinator
            .set_router_and_media_clients_for_test(
                router,
                stt_client,
                ocr_client,
                image_gen_client,
                tts_client,
                media_config,
            )
            .await
    }

    /// Apply cold-start wiring in one pass and rebuild the catalog once.
    ///
    /// Messaging and memory ports are bound before the rebuild. Limits,
    /// security and tool settings are applied to their engines, then one
    /// `PlatformRuntime` snapshot is published with every field.
    pub async fn wire_startup(&self, wiring: StartupWiring) -> anyhow::Result<()> {
        self.coordinator.wire_startup(wiring).await
    }

    /// Run user/project lifecycle hooks for an Agent event. PreToolUse returns
    /// an allow/deny decision; notification events remain best-effort.
    pub async fn run_lifecycle_hooks(
        &self,
        payload: haven_common::hooks::LifecycleHookPayload,
        cancel: CancellationToken,
    ) -> haven_common::hooks::LifecycleHookDecision {
        self.coordinator
            .lifecycle_hooks
            .run(&self.coordinator.core.authorization, payload, cancel)
            .await
    }

    /// Return bounded, content-free runtime diagnostics for lifecycle hooks.
    pub fn lifecycle_hook_diagnostics(
        &self,
    ) -> Vec<haven_common::hooks::LifecycleHookEventDiagnostics> {
        self.coordinator.lifecycle_hooks.diagnostics()
    }

    /// Return the policy currently enforced by the Hook process authorization gate.
    pub async fn lifecycle_hook_resource_policy(&self) -> haven_common::config::ResourcePolicy {
        self.coordinator
            .core
            .authorization
            .lifecycle_hook_resource_policy()
            .await
    }

    /// Apply the security configuration to every runtime boundary that needs
    /// the same snapshot. The authorization engine protects tool execution;
    /// the MCP manager additionally protects startup, refresh, reconnect, and
    /// health-monitor connection paths.
    pub async fn apply_security(&self, security: &SecurityConfig) -> Result<(), String> {
        self.coordinator.apply_security(security).await
    }

    /// Wire the app-level context for the five native admin operations. Called by the
    /// desktop shell after the config loader exists; later catalog rebuilds
    /// keep the capability-scoped adapters registered. Durable ToolRun storage
    /// is injected separately through startup wiring by the composition root.
    #[cfg(test)]
    pub(crate) async fn set_admin_context(
        &self,
        ctx: builtin::AdminContext,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.coordinator.set_admin_context(ctx).await
    }

    pub async fn set_tool_settings(
        &self,
        settings: HashMap<String, ToolConfig>,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.coordinator.set_tool_settings(settings).await
    }

    /// Metadata and execution capability for typed admin requests, when the
    /// desktop shell wired the app context. Provider tools use the same
    /// internally owned operations through their typed adapters.
    pub async fn admin_operations(&self) -> Option<Arc<dyn AdminOperationPort>> {
        self.coordinator
            .runtime
            .builtin_catalog()
            .await
            .admin_operations
            .clone()
            .map(|operation_set| Arc::new(SharedAdminOperationPort { operation_set }) as _)
    }

    /// Flip the `enabled` flag for one builtin tool in the in-memory
    /// `tool_settings` and rebuild the catalog so the toggle takes effect on
    /// the agent's next step. The config.toml persistence is done by the
    /// caller (the admin surface's `tool_enable`/`tool_disable` operations,
    /// which call this after persisting).
    async fn set_tool_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.coordinator.set_tool_enabled(name, enabled).await
    }

    /// Replace the unified context limits (global tool output cap etc.) and
    /// rebuild the catalog so tools pick up the new values.
    pub async fn set_context_limits(
        &self,
        limits: ContextLimitsConfig,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.coordinator.set_context_limits(limits).await
    }

    /// Replace the default shell for the `shell` tool and rebuild the catalog
    /// so the running agent picks up the new value on its next step.
    pub async fn set_default_shell(
        &self,
        shell: ShellChoice,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.coordinator.set_default_shell(shell).await
    }

    /// Snapshot the shell default used by the model-facing `shell` tool.
    pub async fn default_shell_name(&self) -> String {
        self.coordinator
            .runtime
            .platform()
            .await
            .default_shell
            .as_str()
            .to_string()
    }

    /// Snapshot the limits that shape model-visible tool and observation
    /// budgets. Prompt assembly uses this instead of duplicating defaults.
    #[cfg(test)]
    pub(crate) async fn context_limits(&self) -> ContextLimitsConfig {
        self.coordinator
            .runtime
            .platform()
            .await
            .context_limits
            .clone()
    }

    /// Whether the model-facing `media.speak` operation has a live TTS
    /// backend. This is intentionally separate from the media tool's schema
    /// so prompt assembly can report the same capability state.
    #[cfg(test)]
    pub(crate) async fn tts_configured(&self) -> bool {
        let platform = self.coordinator.runtime.platform().await;
        self.tool_capability_snapshot(&platform).await.media.speak
    }

    /// Whether the shared media transcription boundary currently has a live
    /// route. This is the app-facing gate for voice ingress; capture itself is
    /// owned by `haven-input` and is intentionally not consulted here.
    pub async fn transcription_available(&self) -> bool {
        let platform = self.coordinator.runtime.platform().await;
        self.tool_capability_snapshot(&platform)
            .await
            .media
            .transcribe
    }

    /// Transcribe app-captured WAV data through the same media provider
    /// policy used by `media.transcribe`: dedicated STT first, then the LLM
    /// route when the dedicated result is unusable. The input crate never
    /// sees provider clients or fallback decisions.
    pub async fn transcribe_recording(
        &self,
        wav_data: &[u8],
        cancel: CancellationToken,
    ) -> builtin::MediaTranscriptionResult {
        let platform = self.coordinator.runtime.platform().await;
        let model_media = platform
            .model_media_runtime
            .as_ref()
            .map(haven_llm::ModelMediaRuntime::snapshot);
        let capabilities = self
            .tool_capability_snapshot_for(&platform, model_media.as_deref())
            .await;
        if !capabilities.media.transcribe {
            return builtin::MediaTranscriptionResult::unavailable(
                "No speech-to-text provider is configured.",
            );
        }
        let Some(model_media) = model_media else {
            return builtin::MediaTranscriptionResult::unavailable(
                "No speech-to-text provider is configured.",
            );
        };
        let media_config = model_media.media_config().clone();
        let max_output_chars = platform.context_limits.max_observation_chars;
        builtin::media::MediaTranscriber::new(
            Some(model_media.router().clone()),
            model_media.stt_client().cloned(),
            media_config.stt.timeout_secs,
            media_config.stt.min_confidence,
            max_output_chars,
        )
        .transcribe_wav(wav_data, &cancel)
        .await
    }

    /// Return the same live capability decisions used while rebuilding the
    /// builtin catalog. Keeping this at the facade boundary prevents the
    /// prompt snapshot from advertising a role that the tool schema removed.
    pub async fn runtime_capabilities(&self) -> RuntimeCapabilities {
        let platform = self.coordinator.runtime.platform().await;
        self.tool_capability_snapshot(&platform)
            .await
            .project_runtime_capabilities()
    }

    /// Return the immutable backend execution root used by file path
    /// resolution and WorkspaceWrite authorization.
    pub async fn execution_workspace_snapshot(&self) -> ExecutionWorkspaceSnapshot {
        self.coordinator
            .core
            .authorization
            .execution_workspace_snapshot()
            .await
    }

    /// Validate a configured execution root without changing the active
    /// workspace. Settings commands use this before committing the config.
    pub fn validate_execution_workspace(&self, root: &Path) -> Result<PathBuf, String> {
        let workspace = crate::execution_workspace::ExecutionWorkspace::open_root(root, 1)
            .map_err(|_| "workspace root is not an available local directory".to_string())?;
        workspace
            .root()
            .map(Path::to_path_buf)
            .ok_or_else(|| "workspace root is not an available local directory".to_string())
    }

    /// Validate configured resource directories without changing active policy.
    /// Settings commands call this before committing the configuration patch.
    pub fn validate_resource_roots(&self, roots: &[String]) -> Result<(), String> {
        let paths = roots.iter().map(PathBuf::from).collect::<Vec<_>>();
        crate::security::open_directory_roots(&paths, "resource_policy directory roots").map(|_| ())
    }

    /// Replace the active backend execution workspace after validating and
    /// opening its local directory. A successful change increments the
    /// workspace generation and invalidates outstanding execution proofs.
    pub async fn switch_execution_workspace(
        &self,
        root: &Path,
    ) -> anyhow::Result<ExecutionWorkspaceSnapshot> {
        self.coordinator
            .core
            .authorization
            .switch_execution_workspace(root)
            .await
            .map_err(anyhow::Error::msg)
    }

    /// Construct the capability view used by facade reads and builtin catalog
    /// assembly. Model/media inputs come from one captured generation; MCP
    /// discovery remains an independent live input.
    pub(super) async fn tool_capability_snapshot(
        &self,
        platform: &crate::tool_runtime::PlatformRuntime,
    ) -> runtime_capabilities::ToolCapabilitySnapshot {
        let model_media = platform
            .model_media_runtime
            .as_ref()
            .map(haven_llm::ModelMediaRuntime::snapshot);
        self.tool_capability_snapshot_for(platform, model_media.as_deref())
            .await
    }

    async fn tool_capability_snapshot_for(
        &self,
        platform: &crate::tool_runtime::PlatformRuntime,
        model_media: Option<&haven_llm::ModelMediaSnapshot>,
    ) -> runtime_capabilities::ToolCapabilitySnapshot {
        self.coordinator
            .tool_capability_snapshot(platform, model_media)
            .await
    }
}

struct ToolControlHandle(std::sync::Weak<ToolsFacade>);

#[async_trait::async_trait]
impl ToolControlPort for ToolControlHandle {
    async fn set_tool_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()> {
        let tools = self
            .0
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("tool catalog is no longer available"))?;
        tools.set_tool_enabled(name, enabled).await?;
        Ok(())
    }

    async fn rebuild_catalog(&self) -> anyhow::Result<()> {
        let tools = self
            .0
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("tool catalog is no longer available"))?;
        tools.rebuild_catalog().await?;
        Ok(())
    }
}

impl Default for ToolsFacade {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use haven_common::config::{
        Capability, ModelEndpoint, RequestKind, RequestPolicy, RoutedModel, RouterConfig,
    };

    async fn skill_execution_proof(
        tools: &ToolsFacade,
        skill_name: &str,
        params: &Value,
    ) -> (
        crate::AuthorizationRequest,
        crate::AuthorizationExecutionProof,
    ) {
        let authorization = &tools.coordinator.core.authorization;
        authorization
            .set_boundaries(
                haven_common::types::SandboxMode::FullAccess,
                Vec::new(),
                haven_common::types::NetworkPolicy::Open,
            )
            .await;
        authorization
            .set_execution_mode(haven_common::types::ExecutionMode::Collaborative)
            .await;
        let tool_name = crate::skill_tool_name(skill_name);
        let capability = haven_common::types::CapabilityScope::try_new(&tool_name).unwrap();
        let request = crate::AuthorizationRequest::new(
            None,
            tool_name,
            params.clone(),
            crate::OperationPolicy::external(capability, haven_common::types::RiskLevel::High),
        );
        let check = authorization.authorize_for_execution(&request).await;
        let crate::AuthorizationDecision::RequiresConfirmation { receipt, .. } = check.decision
        else {
            panic!("unscoped Skill execution should require a fresh confirmation")
        };
        let proof = authorization
            .confirm_for_execution(&request, &receipt)
            .await
            .expect("the exact user-approved Skill request has an execution proof");
        (request, proof)
    }

    struct AvailableStt;

    #[async_trait::async_trait]
    impl haven_llm::SttClient for AvailableStt {
        async fn transcribe(&self, _wav_data: &[u8]) -> anyhow::Result<haven_llm::SttResult> {
            anyhow::bail!("unused test STT client")
        }
    }

    struct AvailableTts;

    #[async_trait::async_trait]
    impl haven_llm::TtsClient for AvailableTts {
        async fn synthesize(&self, _text: &str) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("unused test TTS client")
        }
    }

    fn provider_search_router() -> Arc<LlmRouter> {
        Arc::new(LlmRouter::new(RouterConfig {
            models: vec![RoutedModel {
                model_config_id: "chat".into(),
                endpoint: ModelEndpoint {
                    provider: "deepseek".into(),
                    api_style: Some("openai-responses".into()),
                    model_name: "deepseek-v4-pro".into(),
                    api_key: "test-key".into(),
                    web_search: Some("auto".into()),
                    ..Default::default()
                },
                capabilities: vec![Capability::Chat],
            }],
            request_policies: vec![RequestPolicy {
                request: RequestKind::Chat,
                primary: "chat".into(),
            }],
            ..Default::default()
        }))
    }

    #[cfg(windows)]
    #[test]
    fn execution_workspace_validation_returns_the_normalized_local_root() {
        let selected = tempfile::tempdir().unwrap();
        let tools = ToolsFacade::new();

        let normalized = tools.validate_execution_workspace(selected.path()).unwrap();
        let expected =
            crate::execution_workspace::ExecutionWorkspace::open_root(selected.path(), 1).unwrap();
        assert_eq!(
            normalized.as_path(),
            expected.root().expect("opened workspace has a root")
        );
        assert!(
            tools
                .validate_execution_workspace(&selected.path().join("missing"))
                .is_err()
        );
    }

    #[test]
    fn resource_root_validation_checks_every_root() {
        let valid = tempfile::tempdir().unwrap();
        let second_valid = tempfile::tempdir().unwrap();
        let tools = ToolsFacade::new();

        assert!(
            tools
                .validate_resource_roots(&[
                    valid.path().to_string_lossy().into_owned(),
                    second_valid.path().to_string_lossy().into_owned(),
                ])
                .is_ok()
        );

        let missing = valid.path().join("missing");
        let error = tools
            .validate_resource_roots(&[
                valid.path().to_string_lossy().into_owned(),
                missing.to_string_lossy().into_owned(),
            ])
            .unwrap_err();
        assert!(error.contains("resource_policy directory roots[1]"));
    }

    #[tokio::test]
    async fn capability_snapshot_tracks_runtime_replacement_across_read_paths() {
        let tools = ToolsFacade::new();
        let initial = tools.runtime_capabilities().await;
        assert_eq!(initial.web_search, WebSearchAvailability::Unavailable);
        assert!(!initial.transcription);
        assert!(!initial.recording);
        assert!(!tools.transcription_available().await);
        assert!(!tools.tts_configured().await);

        tools
            .set_router_and_media_clients(
                provider_search_router(),
                Some(Arc::new(AvailableStt)),
                None,
                None,
                Some(Arc::new(AvailableTts)),
                haven_common::config::MediaConfig::default(),
            )
            .await
            .unwrap();

        let after_config_publish = tools.runtime_capabilities().await;
        assert_eq!(
            after_config_publish.web_search,
            WebSearchAvailability::Provider
        );
        assert!(after_config_publish.transcription);
        assert!(!after_config_publish.recording);
        assert!(tools.transcription_available().await);
        assert!(after_config_publish.tts);
        assert!(tools.tts_configured().await);

        let input_pipeline = Arc::new(haven_input::InputPipeline::new());
        tools
            .coordinator
            .runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.input_pipeline = Some(input_pipeline);
                next
            })
            .await;
        tools.rebuild_catalog().await.unwrap();

        let platform = tools.coordinator.runtime.platform().await;
        let snapshot = tools.tool_capability_snapshot(&platform).await;
        assert!(snapshot.media.record);
        assert!(snapshot.media.transcribe);
        assert_eq!(snapshot.web_search, WebSearchAvailability::Provider);
        let media_catalog = tools.coordinator.runtime.builtin_catalog().await;
        assert!(
            media_catalog
                .tools
                .iter()
                .any(|tool| tool.name() == "media.record")
        );
        assert!(
            media_catalog
                .tools
                .iter()
                .any(|tool| tool.name() == "media.transcribe")
        );

        tools
            .set_router_and_media_clients(
                Arc::new(LlmRouter::new(RouterConfig::default())),
                None,
                None,
                None,
                None,
                haven_common::config::MediaConfig::default(),
            )
            .await
            .unwrap();

        let after_runtime_replacement = tools.runtime_capabilities().await;
        assert_eq!(
            after_runtime_replacement.web_search,
            WebSearchAvailability::Unavailable
        );
        assert!(!after_runtime_replacement.transcription);
        assert!(after_runtime_replacement.recording);
        assert!(!tools.transcription_available().await);
        let transcription = tools
            .transcribe_recording(&[], CancellationToken::new())
            .await;
        assert_eq!(
            transcription.status,
            builtin::MediaTranscriptionStatus::Unavailable
        );
        assert!(!tools.tts_configured().await);
        let media_catalog = tools.coordinator.runtime.builtin_catalog().await;
        assert!(
            media_catalog
                .tools
                .iter()
                .any(|tool| tool.name() == "media.record")
        );
        assert!(
            !media_catalog
                .tools
                .iter()
                .any(|tool| tool.name() == "media.transcribe")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_capability_reads_and_platform_replacements_do_not_panic() {
        let tools = Arc::new(ToolsFacade::new());
        let stt: Arc<dyn haven_llm::SttClient> = Arc::new(AvailableStt);

        let writer_tools = Arc::clone(&tools);
        let writer_stt = Arc::clone(&stt);
        let writer = tokio::spawn(async move {
            for generation in 0..8 {
                let stt = (generation % 2 == 0).then(|| Arc::clone(&writer_stt));
                writer_tools
                    .set_router_and_media_clients(
                        Arc::new(LlmRouter::new(RouterConfig::default())),
                        stt,
                        None,
                        None,
                        None,
                        haven_common::config::MediaConfig::default(),
                    )
                    .await
                    .unwrap();
            }
        });

        let mut readers = Vec::new();
        for _ in 0..4 {
            let tools = Arc::clone(&tools);
            readers.push(tokio::spawn(async move {
                for _ in 0..16 {
                    let capabilities = tools.runtime_capabilities().await;
                    assert!(matches!(
                        capabilities.web_search,
                        WebSearchAvailability::Independent
                            | WebSearchAvailability::Provider
                            | WebSearchAvailability::Mcp
                            | WebSearchAvailability::Unavailable
                    ));
                    let _ = tools.transcription_available().await;
                    let _ = tools.tts_configured().await;
                    let _ = tools
                        .tool_catalog_snapshot("ses-71f81a13c55a5cd3b6672293c3588959")
                        .await;
                }
            }));
        }

        writer.await.expect("platform writer must not panic");
        for reader in readers {
            reader.await.expect("capability reader must not panic");
        }
    }

    #[tokio::test]
    async fn skill_execution_resolves_skills_inside_tools() {
        let tools = ToolsFacade::new();
        let services = tools.share_services();
        let params = serde_json::json!({});
        let (request, proof) = skill_execution_proof(&tools, "missing-skill", &params).await;
        let error = services
            .skill_execution
            .execute_skill(
                "missing-skill",
                &params,
                &request,
                &proof,
                CancellationToken::new(),
            )
            .await
            .expect_err("unknown skills must not reach the runner");

        assert_eq!(error.to_string(), "skill 'missing-skill' not found");
    }

    #[tokio::test]
    async fn skill_execution_rechecks_enabled_state_inside_owner() {
        let tools = ToolsFacade::new();
        let services = tools.share_services();
        let directory = tempfile::tempdir().unwrap();
        let skill_root = directory.path().join("skills");
        let skill_dir = skill_root.join("demo");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: \"demo\"\ndescription: \"test\"\nmetadata:\n  haven.language: \"python\"\n---\n\nRun the test skill.\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("scripts/main.py"), "print('{}')\n").unwrap();
        services
            .skills
            .set_config(Some(skill_root), Some(Vec::new()))
            .await
            .unwrap();

        let params = serde_json::json!({});
        let (request, proof) = skill_execution_proof(&tools, "demo", &params).await;
        let error = services
            .skill_execution
            .execute_skill("demo", &params, &request, &proof, CancellationToken::new())
            .await
            .expect_err("disabled skills must not reach the runner");

        assert_eq!(error.to_string(), "skill 'demo' is disabled");
    }
}
