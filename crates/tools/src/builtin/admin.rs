//! The five capability-scoped administration surfaces.
//!
//! Each surface is an independent [`TypedToolOperation`]. JSON is accepted
//! only by the provider-facing [`TypedToolAdapter`]; native callers keep the
//! typed request and invoke the same operation through [`AdminSurfaces`].
//!
//! The five operation contracts intentionally stay together here: this is the
//! single authority for the capability names, tagged argument schemas, native
//! request bridge, metadata, and adapter construction. Domain side effects
//! live in `admin_services.rs`, keeping this contract module separate from
//! persistence and live-runtime orchestration.

#[path = "admin_services.rs"]
mod admin_services;

use crate::{
    LogLevelPort, OperationIdempotency, ToolBox, ToolCancellationPolicy, ToolConcurrency,
    ToolControlPort, ToolErrorMetadata, ToolExecutionOutcome, ToolOperationMetadata,
    ToolOperationScope, ToolRegistry, ToolResult, TypedToolAdapter, TypedToolOperation,
};
use async_trait::async_trait;
use haven_common::config::{ConfigService, LogLevel, McpServerConfig};
use haven_common::types::{McpTransportType, RiskLevel};
use haven_llm::LlmRouter;
use haven_mcp::{McpManager, McpReconcile};
use haven_memory::{MemoryFactStore, SessionStore};
use haven_skills::SkillsEngine;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub(crate) use admin_services::sanitize_diagnostic;
use admin_services::{
    AdminServices, DiagnosticsStatus, ErrorsOutput, LogsTailOutput, McpAddOutput,
    McpConfigUpdateOutput, McpConnectionOutput, McpRefreshOutput, McpReloadOutput, McpStatusOutput,
    NativeMcpServiceError, SessionsOutput, SkillCreateOutput, SkillSetOutput, SkillsListOutput,
    ToolSetResult,
};

/// App-provided dependencies shared by all five admin surfaces.
#[derive(Clone)]
pub struct AdminContext {
    pub config_service: Option<Arc<ConfigService>>,
    /// Shared with app-owned config apply so admin writes cannot publish a
    /// stale runtime generation while a Settings/model apply is in progress.
    pub config_apply_gate: Option<Arc<tokio::sync::Mutex<()>>>,
    pub session_store: Option<SessionStore>,
    pub memory_facts: Option<MemoryFactStore>,
    pub router: Option<Arc<LlmRouter>>,
    /// Configured base path. `file_logging_enabled` determines whether it may
    /// be read; `None` is reserved for callers without logging context.
    pub log_path: Option<PathBuf>,
    pub file_logging_enabled: bool,
    pub log_level: Option<Arc<dyn LogLevelPort>>,
    pub tool_control: Option<Arc<dyn ToolControlPort>>,
}

/// Narrow context retained for callers that construct only the config
/// operation. It contains no generic operation dispatcher state.
#[derive(Clone)]
pub struct ConfigAdminContext {
    pub config_service: Option<Arc<ConfigService>>,
    pub config_apply_gate: Option<Arc<tokio::sync::Mutex<()>>>,
    pub log_level: Option<Arc<dyn LogLevelPort>>,
}

impl From<ConfigAdminContext> for AdminContext {
    fn from(context: ConfigAdminContext) -> Self {
        Self {
            config_service: context.config_service,
            config_apply_gate: context.config_apply_gate,
            session_store: None,
            memory_facts: None,
            router: None,
            log_path: None,
            file_logging_enabled: false,
            log_level: context.log_level,
            tool_control: None,
        }
    }
}

const MODEL_TOGGLEABLE_TOOL_NAMES: &[&str] = &[
    "media",
    "ask",
    "files",
    "shell",
    "system",
    "http",
    "notify",
    "agent",
    "memory",
    "process",
    "clipboard",
    "input",
    "window",
    "tool_runs",
    "schedule",
    "preferences",
    "checklist",
];

fn is_model_toggleable_tool(name: &str) -> bool {
    MODEL_TOGGLEABLE_TOOL_NAMES.contains(&name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminCapability {
    Diagnostics,
    Config,
    Skills,
    Tools,
    Mcp,
}

impl AdminCapability {
    pub const ALL: [Self; 5] = [
        Self::Diagnostics,
        Self::Config,
        Self::Skills,
        Self::Tools,
        Self::Mcp,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Diagnostics => "haven_diagnostics",
            Self::Config => "haven_config",
            Self::Skills => "haven_skills",
            Self::Tools => "haven_tools",
            Self::Mcp => "haven_mcp",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Diagnostics => crate::prompts::DIAGNOSTICS_DESCRIPTION,
            Self::Config => crate::prompts::CONFIG_DESCRIPTION,
            Self::Skills => crate::prompts::SKILLS_DESCRIPTION,
            Self::Tools => crate::prompts::TOOLS_DESCRIPTION,
            Self::Mcp => crate::prompts::MCP_DESCRIPTION,
        }
    }
}

/// Diagnostics arguments are a closed operation union. Unknown fields and
/// cross-operation fields are rejected by serde before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosticsOperationArgs {
    Status,
    LogsTail {
        #[serde(default)]
        limit: Option<i64>,
    },
    Sessions {
        #[serde(default)]
        limit: Option<i64>,
    },
    Errors {
        #[serde(default)]
        limit: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum SkillsOperationArgs {
    SkillsList,
    SkillEnable {
        name: String,
    },
    SkillDisable {
        name: String,
    },
    SkillCreate {
        name: String,
        description: String,
        instructions: String,
        #[serde(default)]
        language: Option<String>,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        script: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolsOperationArgs {
    ToolEnable { name: String },
    ToolDisable { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpOperationArgs {
    McpList,
    McpConnect {
        name: String,
    },
    McpDisconnect {
        name: String,
    },
    McpAdd {
        name: String,
        transport: McpTransportType,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default = "default_enabled")]
        enabled: bool,
        #[serde(default = "default_enabled")]
        auto_connect: bool,
    },
    McpUpdate {
        name: String,
        #[serde(default)]
        transport: Option<McpTransportType>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        args: Option<Vec<String>>,
        #[serde(default)]
        env: Option<Vec<String>>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        enabled: Option<bool>,
    },
    McpToggle {
        name: String,
        enabled: bool,
    },
    McpRemove {
        name: String,
    },
    McpReload,
}

/// A typed authorization payload used only by renderer-facing MCP management
/// commands. These operations intentionally do not appear in
/// [`McpOperationArgs`] or the model-visible `haven_mcp` JSON schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeMcpOperationArgs {
    McpReconnect { name: String, config_version: u64 },
    McpRefresh { plan: McpRefreshPlan },
}

/// The backend-derived set of connection changes shown to the user before a
/// renderer-triggered diff refresh. It contains no commands, URLs, arguments,
/// environment values, or other connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRefreshPlan {
    pub config_version: u64,
    pub targets: Vec<McpRefreshTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRefreshTarget {
    pub name: String,
    pub action: McpRefreshAction,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum McpRefreshAction {
    Connect,
    Reconnect,
    Disconnect,
}

impl McpRefreshPlan {
    pub fn from_reconcile(config_version: u64, reconcile: &McpReconcile) -> Self {
        let mut targets = Vec::new();
        targets.extend(
            reconcile
                .to_connect_new
                .iter()
                .map(|server| McpRefreshTarget {
                    name: server.name.clone(),
                    action: McpRefreshAction::Connect,
                }),
        );
        targets.extend(
            reconcile
                .to_connect_changed
                .iter()
                .map(|server| McpRefreshTarget {
                    name: server.name.clone(),
                    action: McpRefreshAction::Reconnect,
                }),
        );
        targets.extend(reconcile.to_remove.iter().map(|name| McpRefreshTarget {
            name: name.clone(),
            action: McpRefreshAction::Disconnect,
        }));
        targets.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.action.cmp(&right.action))
        });
        Self {
            config_version,
            targets,
        }
    }

    pub fn requires_network(&self) -> bool {
        self.targets.iter().any(|target| {
            matches!(
                target.action,
                McpRefreshAction::Connect | McpRefreshAction::Reconnect
            )
        })
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfigOperationArgs {
    ConfigGet {
        #[serde(default)]
        path: Option<String>,
    },
    LogsLevel {
        level: LogLevel,
    },
}

#[derive(Debug, Clone)]
pub struct McpAddFields {
    pub name: String,
    pub transport: McpTransportType,
    pub command: Option<String>,
    pub url: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub cwd: Option<String>,
    pub enabled: bool,
    pub auto_connect: bool,
}

#[derive(Debug, Clone)]
pub struct McpUpdateFields {
    pub name: String,
    pub transport: Option<McpTransportType>,
    pub command: Option<String>,
    pub url: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub enabled: Option<bool>,
}

/// The native confirmation queue stores this typed union. It is only a host
/// bridge; each variant is executed by its own typed surface below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AdminRequest {
    Diagnostics(DiagnosticsOperationArgs),
    Config(ConfigOperationArgs),
    Skills(SkillsOperationArgs),
    Tools(ToolsOperationArgs),
    Mcp(McpOperationArgs),
    NativeMcp(NativeMcpOperationArgs),
}

impl AdminRequest {
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Diagnostics(_) => "haven_diagnostics",
            Self::Config(_) => "haven_config",
            Self::Skills(_) => "haven_skills",
            Self::Tools(_) => "haven_tools",
            Self::Mcp(_) => "haven_mcp",
            Self::NativeMcp(_) => "haven_mcp",
        }
    }

    pub fn model_operation_name(&self) -> &'static str {
        match self {
            Self::Diagnostics(DiagnosticsOperationArgs::Status) => "haven.diagnostics.status",
            Self::Diagnostics(DiagnosticsOperationArgs::LogsTail { .. }) => {
                "haven.diagnostics.logs_tail"
            }
            Self::Diagnostics(DiagnosticsOperationArgs::Sessions { .. }) => {
                "haven.diagnostics.sessions"
            }
            Self::Diagnostics(DiagnosticsOperationArgs::Errors { .. }) => {
                "haven.diagnostics.errors"
            }
            Self::Config(ConfigOperationArgs::ConfigGet { .. }) => "haven.config.config_get",
            Self::Config(ConfigOperationArgs::LogsLevel { .. }) => "haven.config.logs_level",
            Self::Skills(SkillsOperationArgs::SkillsList) => "haven.skills.skills_list",
            Self::Skills(SkillsOperationArgs::SkillEnable { .. }) => "haven.skills.skill_enable",
            Self::Skills(SkillsOperationArgs::SkillDisable { .. }) => "haven.skills.skill_disable",
            Self::Skills(SkillsOperationArgs::SkillCreate { .. }) => "haven.skills.skill_create",
            Self::Tools(ToolsOperationArgs::ToolEnable { .. }) => "haven.tools.tool_enable",
            Self::Tools(ToolsOperationArgs::ToolDisable { .. }) => "haven.tools.tool_disable",
            Self::Mcp(McpOperationArgs::McpList) => "haven.mcp.mcp_list",
            Self::Mcp(McpOperationArgs::McpConnect { .. }) => "haven.mcp.mcp_connect",
            Self::Mcp(McpOperationArgs::McpDisconnect { .. }) => "haven.mcp.mcp_disconnect",
            Self::Mcp(McpOperationArgs::McpAdd { .. }) => "haven.mcp.mcp_add",
            Self::Mcp(McpOperationArgs::McpUpdate { .. }) => "haven.mcp.mcp_update",
            Self::Mcp(McpOperationArgs::McpToggle { .. }) => "haven.mcp.mcp_toggle",
            Self::Mcp(McpOperationArgs::McpRemove { .. }) => "haven.mcp.mcp_remove",
            Self::Mcp(McpOperationArgs::McpReload) => "haven.mcp.mcp_reload",
            Self::NativeMcp(NativeMcpOperationArgs::McpReconnect { .. }) => {
                "haven.mcp.mcp_reconnect"
            }
            Self::NativeMcp(NativeMcpOperationArgs::McpRefresh { .. }) => "haven.mcp.mcp_refresh",
        }
    }

    /// Network metadata for model-visible and native MCP management requests
    /// comes from the shared operation contract. Native refresh is opaque only
    /// when its backend-derived diff will connect.
    pub fn network_access(&self) -> crate::NetworkAccess {
        match self {
            Self::NativeMcp(NativeMcpOperationArgs::McpReconnect { .. }) => {
                crate::NetworkAccess::Opaque
            }
            Self::NativeMcp(NativeMcpOperationArgs::McpRefresh { plan }) => {
                if plan.requires_network() {
                    crate::NetworkAccess::Opaque
                } else {
                    crate::NetworkAccess::None
                }
            }
            Self::Mcp(_) => {
                super::operation_contract::operation_contract(self.model_operation_name())
                    .network_access_override
                    .unwrap_or(crate::NetworkAccess::Opaque)
            }
            _ => crate::NetworkAccess::None,
        }
    }

    pub fn input(&self) -> Value {
        match self {
            Self::Diagnostics(args) => {
                serde_json::to_value(args).expect("admin request arguments are serializable")
            }
            Self::Config(args) => {
                serde_json::to_value(args).expect("admin request arguments are serializable")
            }
            Self::Skills(args) => {
                serde_json::to_value(args).expect("admin request arguments are serializable")
            }
            Self::Tools(args) => {
                serde_json::to_value(args).expect("admin request arguments are serializable")
            }
            Self::Mcp(args) => {
                serde_json::to_value(args).expect("admin request arguments are serializable")
            }
            Self::NativeMcp(args) => {
                serde_json::to_value(args).expect("native MCP request arguments are serializable")
            }
        }
    }

    pub fn server_name(&self) -> Option<&str> {
        match self {
            Self::Mcp(McpOperationArgs::McpConnect { name })
            | Self::Mcp(McpOperationArgs::McpDisconnect { name })
            | Self::Mcp(McpOperationArgs::McpToggle { name, .. })
            | Self::Mcp(McpOperationArgs::McpRemove { name })
            | Self::Mcp(McpOperationArgs::McpAdd { name, .. })
            | Self::Mcp(McpOperationArgs::McpUpdate { name, .. }) => Some(name),
            Self::NativeMcp(NativeMcpOperationArgs::McpReconnect { name, .. }) => Some(name),
            _ => None,
        }
    }
}

/// Closed set of fixed-shape admin responses. The untagged representation
/// keeps each operation's established JSON shape at the tool-output boundary.
#[derive(Debug, Clone)]
pub struct AdminOperationOutput(AdminOperationOutputKind);

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum AdminOperationOutputKind {
    DiagnosticsStatus(DiagnosticsStatus),
    LogsTail(LogsTailOutput),
    Sessions(SessionsOutput),
    Errors(ErrorsOutput),
    SkillsList(SkillsListOutput),
    SkillSet(SkillSetOutput),
    SkillCreate(SkillCreateOutput),
    ToolSet(ToolSetResult),
    McpStatus(Vec<McpStatusOutput>),
    McpConnection(McpConnectionOutput),
    McpAdd(McpAddOutput),
    McpConfigUpdate(McpConfigUpdateOutput),
    McpRemove(admin_services::McpRemoveOutput),
    McpReload(McpReloadOutput),
    McpRefresh(McpRefreshOutput),
}

impl AdminOperationOutput {
    fn diagnostics_status(output: DiagnosticsStatus) -> Self {
        Self(AdminOperationOutputKind::DiagnosticsStatus(output))
    }
    fn logs_tail(output: LogsTailOutput) -> Self {
        Self(AdminOperationOutputKind::LogsTail(output))
    }
    fn sessions(output: SessionsOutput) -> Self {
        Self(AdminOperationOutputKind::Sessions(output))
    }
    fn errors(output: ErrorsOutput) -> Self {
        Self(AdminOperationOutputKind::Errors(output))
    }
    fn skills_list(output: SkillsListOutput) -> Self {
        Self(AdminOperationOutputKind::SkillsList(output))
    }
    fn skill_set(output: SkillSetOutput) -> Self {
        Self(AdminOperationOutputKind::SkillSet(output))
    }
    fn skill_create(output: SkillCreateOutput) -> Self {
        Self(AdminOperationOutputKind::SkillCreate(output))
    }
    fn tool_set(output: ToolSetResult) -> Self {
        Self(AdminOperationOutputKind::ToolSet(output))
    }
    fn mcp_status(output: Vec<McpStatusOutput>) -> Self {
        Self(AdminOperationOutputKind::McpStatus(output))
    }
    fn mcp_connection(output: McpConnectionOutput) -> Self {
        Self(AdminOperationOutputKind::McpConnection(output))
    }
    fn mcp_add(output: McpAddOutput) -> Self {
        Self(AdminOperationOutputKind::McpAdd(output))
    }
    fn mcp_config_update(output: McpConfigUpdateOutput) -> Self {
        Self(AdminOperationOutputKind::McpConfigUpdate(output))
    }
    fn mcp_remove(output: admin_services::McpRemoveOutput) -> Self {
        Self(AdminOperationOutputKind::McpRemove(output))
    }
    fn mcp_reload(output: McpReloadOutput) -> Self {
        Self(AdminOperationOutputKind::McpReload(output))
    }
    fn mcp_refresh(output: McpRefreshOutput) -> Self {
        Self(AdminOperationOutputKind::McpRefresh(output))
    }
}

impl Serialize for AdminOperationOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdminErrorKind {
    Cancelled,
    Validation,
    SideEffectMayHaveHappened,
    Other,
}

#[derive(Debug, Clone)]
pub struct AdminOperationError {
    kind: AdminErrorKind,
    message: String,
}

impl AdminOperationError {
    fn cancelled() -> Self {
        Self {
            kind: AdminErrorKind::Cancelled,
            message: "admin operation cancelled".into(),
        }
    }
    fn validation(message: impl Into<String>) -> Self {
        Self {
            kind: AdminErrorKind::Validation,
            message: message.into(),
        }
    }
    fn other(error: impl Into<String>) -> Self {
        Self {
            kind: AdminErrorKind::Other,
            message: sanitize_diagnostic(&error.into()),
        }
    }

    fn side_effect_may_have_happened(error: impl Into<String>) -> Self {
        Self {
            kind: AdminErrorKind::SideEffectMayHaveHappened,
            message: sanitize_diagnostic(&error.into()),
        }
    }
}

impl Display for AdminOperationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn service_error(error: anyhow::Error) -> AdminOperationError {
    AdminOperationError::other(error.to_string())
}

fn side_effect_service_error(error: anyhow::Error) -> AdminOperationError {
    AdminOperationError::side_effect_may_have_happened(error.to_string())
}

fn native_mcp_service_error(error: NativeMcpServiceError) -> AdminOperationError {
    match error {
        NativeMcpServiceError::Preflight(message) => AdminOperationError::validation(message),
        NativeMcpServiceError::BeforeSideEffect(error) => service_error(error),
        NativeMcpServiceError::SideEffect(error) => side_effect_service_error(error),
    }
}

fn metadata(
    capability: &'static str,
    operation: &'static str,
    risk_level: RiskLevel,
    idempotency: OperationIdempotency,
    concurrency: ToolConcurrency,
) -> ToolOperationMetadata {
    ToolOperationMetadata {
        capability,
        operation,
        scope: ToolOperationScope::Global,
        risk_level,
        idempotency,
        cancellation: ToolCancellationPolicy::Terminating,
        timeout_secs: 10,
        concurrency,
    }
}

fn output_schema(branches: Vec<Value>) -> Value {
    serde_json::json!({"type": "object", "oneOf": branches})
}

fn branch(operation: &str, properties: Value, required: &[&str]) -> Value {
    let mut all = Map::new();
    all.insert("operation".into(), serde_json::json!({"const": operation}));
    if let Value::Object(properties) = properties {
        all.extend(properties);
    }
    serde_json::json!({"type": "object", "additionalProperties": false, "properties": all, "required": required})
}

fn operation_error_metadata(error: &AdminOperationError) -> ToolErrorMetadata {
    match error.kind {
        AdminErrorKind::Cancelled => ToolErrorMetadata {
            class: crate::ToolErrorClass::UnknownOutcome,
            outcome: ToolExecutionOutcome::Cancelled,
            retryability: crate::ToolRetryability::Unknown,
        },
        AdminErrorKind::Validation => ToolErrorMetadata::validation(),
        AdminErrorKind::SideEffectMayHaveHappened => ToolErrorMetadata {
            class: crate::ToolErrorClass::SideEffectMayHaveHappened,
            outcome: ToolExecutionOutcome::Failed,
            retryability: crate::ToolRetryability::Unknown,
        },
        AdminErrorKind::Other => ToolErrorMetadata::other(),
    }
}

#[derive(Clone)]
pub struct DiagnosticsAdminOperation {
    services: Arc<AdminServices>,
}
impl DiagnosticsAdminOperation {
    fn new(services: Arc<AdminServices>) -> Self {
        Self { services }
    }
}

#[async_trait]
impl TypedToolOperation for DiagnosticsAdminOperation {
    type Args = DiagnosticsOperationArgs;
    type Output = AdminOperationOutput;
    type Error = AdminOperationError;

    fn metadata(&self, args: &Self::Args) -> ToolOperationMetadata {
        match args {
            DiagnosticsOperationArgs::Status | DiagnosticsOperationArgs::LogsTail { .. } => {
                metadata(
                    "haven_diagnostics",
                    if matches!(args, DiagnosticsOperationArgs::Status) {
                        "status"
                    } else {
                        "logs_tail"
                    },
                    RiskLevel::Low,
                    OperationIdempotency::Idempotent,
                    ToolConcurrency::SharedResource("haven:diagnostics".into()),
                )
            }
            DiagnosticsOperationArgs::Sessions { .. } => metadata(
                "haven_diagnostics",
                "sessions",
                RiskLevel::Low,
                OperationIdempotency::Idempotent,
                ToolConcurrency::SharedResource("haven:sessions".into()),
            ),
            DiagnosticsOperationArgs::Errors { .. } => metadata(
                "haven_diagnostics",
                "errors",
                RiskLevel::Low,
                OperationIdempotency::Idempotent,
                ToolConcurrency::SharedResource("haven:sessions".into()),
            ),
        }
    }
    fn default_metadata(&self) -> ToolOperationMetadata {
        metadata(
            "haven_diagnostics",
            "status",
            RiskLevel::High,
            OperationIdempotency::Unknown,
            ToolConcurrency::Exclusive,
        )
    }
    fn input_schema(&self) -> Value {
        output_schema(vec![
            branch("status", serde_json::json!({}), &["operation"]),
            branch(
                "logs_tail",
                serde_json::json!({"limit": {"type": "integer", "minimum": 1, "maximum": 500}}),
                &["operation"],
            ),
            branch(
                "sessions",
                serde_json::json!({"limit": {"type": "integer", "minimum": 1, "maximum": 50}}),
                &["operation"],
            ),
            branch(
                "errors",
                serde_json::json!({"limit": {"type": "integer", "minimum": 1, "maximum": 50}}),
                &["operation"],
            ),
        ])
    }
    fn error_metadata(&self, error: &Self::Error) -> ToolErrorMetadata {
        operation_error_metadata(error)
    }
    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        if cancel.is_cancelled() {
            return Err(AdminOperationError::cancelled());
        }
        match args {
            DiagnosticsOperationArgs::Status => self
                .services
                .diagnostics_status()
                .await
                .map(AdminOperationOutput::diagnostics_status)
                .map_err(service_error),
            DiagnosticsOperationArgs::LogsTail { limit } => self
                .services
                .logs_tail(limit)
                .await
                .map(AdminOperationOutput::logs_tail)
                .map_err(service_error),
            DiagnosticsOperationArgs::Sessions { limit } => self
                .services
                .sessions(limit)
                .await
                .map(AdminOperationOutput::sessions)
                .map_err(service_error),
            DiagnosticsOperationArgs::Errors { limit } => self
                .services
                .errors(limit)
                .await
                .map(AdminOperationOutput::errors)
                .map_err(service_error),
        }
    }
}

#[derive(Clone)]
pub struct SkillsAdminOperation {
    services: Arc<AdminServices>,
}
impl SkillsAdminOperation {
    fn new(services: Arc<AdminServices>) -> Self {
        Self { services }
    }
}

#[async_trait]
impl TypedToolOperation for SkillsAdminOperation {
    type Args = SkillsOperationArgs;
    type Output = AdminOperationOutput;
    type Error = AdminOperationError;
    fn metadata(&self, args: &Self::Args) -> ToolOperationMetadata {
        match args {
            SkillsOperationArgs::SkillsList => metadata(
                "haven_skills",
                "skills_list",
                RiskLevel::Low,
                OperationIdempotency::Idempotent,
                ToolConcurrency::SharedResource("skills".into()),
            ),
            SkillsOperationArgs::SkillEnable { .. } => metadata(
                "haven_skills",
                "skill_enable",
                RiskLevel::Medium,
                OperationIdempotency::Idempotent,
                ToolConcurrency::Resource("skills".into()),
            ),
            SkillsOperationArgs::SkillDisable { .. } => metadata(
                "haven_skills",
                "skill_disable",
                RiskLevel::Medium,
                OperationIdempotency::Idempotent,
                ToolConcurrency::Resource("skills".into()),
            ),
            SkillsOperationArgs::SkillCreate { .. } => metadata(
                "haven_skills",
                "skill_create",
                RiskLevel::High,
                OperationIdempotency::Unknown,
                ToolConcurrency::Resource("skills".into()),
            ),
        }
    }
    fn default_metadata(&self) -> ToolOperationMetadata {
        metadata(
            "haven_skills",
            "skill_create",
            RiskLevel::High,
            OperationIdempotency::Unknown,
            ToolConcurrency::Exclusive,
        )
    }
    fn input_schema(&self) -> Value {
        output_schema(vec![
            branch("skills_list", serde_json::json!({}), &["operation"]),
            branch(
                "skill_enable",
                serde_json::json!({"name": {"type": "string", "minLength": 1}}),
                &["operation", "name"],
            ),
            branch(
                "skill_disable",
                serde_json::json!({"name": {"type": "string", "minLength": 1}}),
                &["operation", "name"],
            ),
            branch(
                "skill_create",
                serde_json::json!({"name": {"type": "string", "minLength": 1}, "description": {"type": "string", "minLength": 1}, "instructions": {"type": "string", "minLength": 1}, "language": {"type": "string", "enum": ["python"]}, "version": {"type": "string"}, "script": {"type": "string", "minLength": 1}}),
                &["operation", "name", "description", "instructions", "script"],
            ),
        ])
    }
    fn error_metadata(&self, error: &Self::Error) -> ToolErrorMetadata {
        operation_error_metadata(error)
    }
    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        if cancel.is_cancelled() {
            return Err(AdminOperationError::cancelled());
        }
        match args {
            SkillsOperationArgs::SkillsList => self
                .services
                .skills_list()
                .await
                .map(AdminOperationOutput::skills_list)
                .map_err(service_error),
            SkillsOperationArgs::SkillEnable { name } => self
                .services
                .skill_set(&name, true)
                .await
                .map(AdminOperationOutput::skill_set)
                .map_err(side_effect_service_error),
            SkillsOperationArgs::SkillDisable { name } => self
                .services
                .skill_set(&name, false)
                .await
                .map(AdminOperationOutput::skill_set)
                .map_err(side_effect_service_error),
            SkillsOperationArgs::SkillCreate {
                name,
                description,
                instructions,
                language,
                version,
                script,
            } => self
                .services
                .skill_create(
                    &name,
                    &description,
                    &instructions,
                    language.as_deref(),
                    version.as_deref(),
                    script.as_deref(),
                )
                .await
                .map(AdminOperationOutput::skill_create)
                .map_err(side_effect_service_error),
        }
    }
}

#[derive(Clone)]
pub struct ToolsAdminOperation {
    services: Arc<AdminServices>,
}
impl ToolsAdminOperation {
    fn new(services: Arc<AdminServices>) -> Self {
        Self { services }
    }
}

#[async_trait]
impl TypedToolOperation for ToolsAdminOperation {
    type Args = ToolsOperationArgs;
    type Output = AdminOperationOutput;
    type Error = AdminOperationError;
    fn metadata(&self, args: &Self::Args) -> ToolOperationMetadata {
        let operation = match args {
            ToolsOperationArgs::ToolEnable { .. } => "tool_enable",
            ToolsOperationArgs::ToolDisable { .. } => "tool_disable",
        };
        metadata(
            "haven_tools",
            operation,
            RiskLevel::Medium,
            OperationIdempotency::Idempotent,
            ToolConcurrency::Resource("tool_settings".into()),
        )
    }
    fn default_metadata(&self) -> ToolOperationMetadata {
        metadata(
            "haven_tools",
            "tool_enable",
            RiskLevel::High,
            OperationIdempotency::Unknown,
            ToolConcurrency::Exclusive,
        )
    }
    fn input_schema(&self) -> Value {
        output_schema(vec![
            branch(
                "tool_enable",
                serde_json::json!({"name": {"type": "string", "enum": MODEL_TOGGLEABLE_TOOL_NAMES}}),
                &["operation", "name"],
            ),
            branch(
                "tool_disable",
                serde_json::json!({"name": {"type": "string", "enum": MODEL_TOGGLEABLE_TOOL_NAMES}}),
                &["operation", "name"],
            ),
        ])
    }
    fn error_metadata(&self, error: &Self::Error) -> ToolErrorMetadata {
        operation_error_metadata(error)
    }
    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        if cancel.is_cancelled() {
            return Err(AdminOperationError::cancelled());
        }
        let (name, enabled) = match args {
            ToolsOperationArgs::ToolEnable { name } => (name, true),
            ToolsOperationArgs::ToolDisable { name } => (name, false),
        };
        if !is_model_toggleable_tool(&name) {
            return Err(AdminOperationError::validation(format!(
                "tool '{}' cannot be changed through the model administration surface",
                name
            )));
        }
        self.services
            .tool_set(&name, enabled)
            .await
            .map(AdminOperationOutput::tool_set)
            .map_err(side_effect_service_error)
    }
}

#[derive(Clone)]
pub struct McpAdminOperation {
    services: Arc<AdminServices>,
}
impl McpAdminOperation {
    fn new(services: Arc<AdminServices>) -> Self {
        Self { services }
    }

    fn native_metadata(&self, args: &NativeMcpOperationArgs) -> ToolOperationMetadata {
        let operation = match args {
            NativeMcpOperationArgs::McpReconnect { .. } => "mcp_reconnect",
            NativeMcpOperationArgs::McpRefresh { .. } => "mcp_refresh",
        };
        metadata(
            "haven_mcp",
            operation,
            RiskLevel::Medium,
            OperationIdempotency::Idempotent,
            ToolConcurrency::Resource("mcp".into()),
        )
    }

    async fn execute_native(
        &self,
        args: NativeMcpOperationArgs,
        cancel: CancellationToken,
    ) -> Result<AdminOperationOutput, AdminOperationError> {
        if cancel.is_cancelled() {
            return Err(AdminOperationError::cancelled());
        }
        match args {
            NativeMcpOperationArgs::McpReconnect {
                name,
                config_version,
            } => self
                .services
                .mcp_reconnect(&name, config_version)
                .await
                .map(AdminOperationOutput::mcp_connection)
                .map_err(native_mcp_service_error),
            NativeMcpOperationArgs::McpRefresh { plan } => self
                .services
                .mcp_refresh(&plan)
                .await
                .map(AdminOperationOutput::mcp_refresh)
                .map_err(native_mcp_service_error),
        }
    }
}

#[async_trait]
impl TypedToolOperation for McpAdminOperation {
    type Args = McpOperationArgs;
    type Output = AdminOperationOutput;
    type Error = AdminOperationError;
    fn metadata(&self, args: &Self::Args) -> ToolOperationMetadata {
        let (operation, risk, idempotency, concurrency) = match args {
            McpOperationArgs::McpList => (
                "mcp_list",
                RiskLevel::Low,
                OperationIdempotency::Idempotent,
                ToolConcurrency::SharedResource("mcp".into()),
            ),
            McpOperationArgs::McpConnect { .. } => (
                "mcp_connect",
                RiskLevel::Medium,
                OperationIdempotency::Idempotent,
                ToolConcurrency::Resource("mcp".into()),
            ),
            McpOperationArgs::McpDisconnect { .. } => (
                "mcp_disconnect",
                RiskLevel::Medium,
                OperationIdempotency::Idempotent,
                ToolConcurrency::Resource("mcp".into()),
            ),
            McpOperationArgs::McpAdd { .. } => (
                "mcp_add",
                RiskLevel::High,
                OperationIdempotency::Unknown,
                ToolConcurrency::Resource("mcp".into()),
            ),
            McpOperationArgs::McpUpdate { .. } => (
                "mcp_update",
                RiskLevel::High,
                OperationIdempotency::Unknown,
                ToolConcurrency::Resource("mcp".into()),
            ),
            McpOperationArgs::McpToggle { .. } => (
                "mcp_toggle",
                RiskLevel::High,
                OperationIdempotency::Idempotent,
                ToolConcurrency::Resource("mcp".into()),
            ),
            McpOperationArgs::McpRemove { .. } => (
                "mcp_remove",
                RiskLevel::High,
                OperationIdempotency::Unknown,
                ToolConcurrency::Resource("mcp".into()),
            ),
            McpOperationArgs::McpReload => (
                "mcp_reload",
                RiskLevel::Medium,
                OperationIdempotency::Idempotent,
                ToolConcurrency::Resource("mcp".into()),
            ),
        };
        metadata("haven_mcp", operation, risk, idempotency, concurrency)
    }
    fn default_metadata(&self) -> ToolOperationMetadata {
        metadata(
            "haven_mcp",
            "mcp_remove",
            RiskLevel::High,
            OperationIdempotency::Unknown,
            ToolConcurrency::Exclusive,
        )
    }
    fn input_schema(&self) -> Value {
        output_schema(vec![
            branch("mcp_list", serde_json::json!({}), &["operation"]),
            branch(
                "mcp_connect",
                serde_json::json!({"name": {"type": "string", "minLength": 1}}),
                &["operation", "name"],
            ),
            branch(
                "mcp_disconnect",
                serde_json::json!({"name": {"type": "string", "minLength": 1}}),
                &["operation", "name"],
            ),
            branch(
                "mcp_add",
                serde_json::json!({"name": {"type": "string", "minLength": 1}, "transport": {"const": "stdio"}, "command": {"type": "string", "minLength": 1}, "args": {"type": "array", "items": {"type": "string"}}, "env": {"type": "array", "items": {"type": "string"}}, "cwd": {"type": "string"}, "enabled": {"type": "boolean"}, "auto_connect": {"type": "boolean"}}),
                &["operation", "name", "transport", "command"],
            ),
            branch(
                "mcp_add",
                serde_json::json!({"name": {"type": "string", "minLength": 1}, "transport": {"const": "http"}, "url": {"type": "string", "minLength": 1}, "enabled": {"type": "boolean"}, "auto_connect": {"type": "boolean"}}),
                &["operation", "name", "transport", "url"],
            ),
            branch(
                "mcp_update",
                serde_json::json!({"name": {"type": "string", "minLength": 1}, "transport": {"enum": ["stdio", "http"]}, "command": {"type": "string"}, "url": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}}, "env": {"type": "array", "items": {"type": "string"}}, "cwd": {"type": "string"}, "enabled": {"type": "boolean"}}),
                &["operation", "name"],
            ),
            branch(
                "mcp_toggle",
                serde_json::json!({"name": {"type": "string", "minLength": 1}, "enabled": {"type": "boolean"}}),
                &["operation", "name", "enabled"],
            ),
            branch(
                "mcp_remove",
                serde_json::json!({"name": {"type": "string", "minLength": 1}}),
                &["operation", "name"],
            ),
            branch("mcp_reload", serde_json::json!({}), &["operation"]),
        ])
    }
    fn error_metadata(&self, error: &Self::Error) -> ToolErrorMetadata {
        operation_error_metadata(error)
    }
    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        if cancel.is_cancelled() {
            return Err(AdminOperationError::cancelled());
        }
        match args {
            McpOperationArgs::McpList => self
                .services
                .mcp_status()
                .await
                .map(AdminOperationOutput::mcp_status)
                .map_err(service_error),
            McpOperationArgs::McpConnect { name } => self
                .services
                .mcp_connect(&name)
                .await
                .map(AdminOperationOutput::mcp_connection)
                .map_err(side_effect_service_error),
            McpOperationArgs::McpDisconnect { name } => self
                .services
                .mcp_disconnect(&name)
                .await
                .map(AdminOperationOutput::mcp_connection)
                .map_err(service_error),
            McpOperationArgs::McpAdd {
                name,
                transport,
                command,
                url,
                args,
                env,
                cwd,
                enabled,
                auto_connect,
            } => self
                .services
                .mcp_add(&McpAddFields {
                    name,
                    transport,
                    command,
                    url,
                    args,
                    env,
                    cwd,
                    enabled,
                    auto_connect,
                })
                .await
                .map(AdminOperationOutput::mcp_add)
                .map_err(side_effect_service_error),
            McpOperationArgs::McpUpdate {
                name,
                transport,
                command,
                url,
                args,
                env,
                cwd,
                enabled,
            } => self
                .services
                .mcp_update(&McpUpdateFields {
                    name,
                    transport,
                    command,
                    url,
                    args,
                    env,
                    cwd,
                    enabled,
                })
                .await
                .map(AdminOperationOutput::mcp_config_update)
                .map_err(side_effect_service_error),
            McpOperationArgs::McpToggle { name, enabled } => self
                .services
                .mcp_toggle(&name, enabled)
                .await
                .map(AdminOperationOutput::mcp_config_update)
                .map_err(side_effect_service_error),
            McpOperationArgs::McpRemove { name } => self
                .services
                .mcp_remove(&name)
                .await
                .map(AdminOperationOutput::mcp_remove)
                .map_err(service_error),
            McpOperationArgs::McpReload => self
                .services
                .mcp_reload()
                .await
                .map(AdminOperationOutput::mcp_reload)
                .map_err(side_effect_service_error),
        }
    }
}

#[derive(Clone)]
pub struct ConfigAdminOperation {
    services: Arc<AdminServices>,
}

impl ConfigAdminOperation {
    pub fn new(context: ConfigAdminContext) -> Self {
        Self {
            services: Arc::new(AdminServices::new(
                context.into(),
                SkillsEngine::new(),
                Arc::new(McpManager::new()),
                Arc::new(RwLock::new(HashMap::new())),
                ToolRegistry::new(),
                0,
                0,
            )),
        }
    }
    fn from_services(services: Arc<AdminServices>) -> Self {
        Self { services }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigViewOutput {
    pub value: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLevelOutput {
    pub level: LogLevel,
    pub saved: bool,
    pub version: u64,
}

#[derive(Debug, Clone)]
pub enum ConfigOperationOutput {
    Config(ConfigViewOutput),
    LogsLevel(LogLevelOutput),
}

impl Serialize for ConfigOperationOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Config(output) => output.value.serialize(serializer),
            Self::LogsLevel(output) => output.serialize(serializer),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ConfigOperationError {
    Cancelled,
    Unavailable,
    PathNotFound { path: String },
    SideEffectMayHaveHappened,
    Failed,
}

impl Display for ConfigOperationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("configuration operation cancelled"),
            Self::Unavailable => formatter.write_str("configuration administration is unavailable"),
            Self::PathNotFound { path } => write!(formatter, "config key '{}' not found", path),
            Self::SideEffectMayHaveHappened => {
                formatter.write_str("configuration log level may have been changed")
            }
            Self::Failed => formatter.write_str("configuration operation failed"),
        }
    }
}

impl From<ConfigOperationError> for AdminOperationError {
    fn from(error: ConfigOperationError) -> Self {
        match error {
            ConfigOperationError::Cancelled => Self::cancelled(),
            ConfigOperationError::PathNotFound { path } => {
                Self::validation(format!("config key '{}' not found", path))
            }
            ConfigOperationError::SideEffectMayHaveHappened => {
                Self::side_effect_may_have_happened(error.to_string())
            }
            ConfigOperationError::Unavailable | ConfigOperationError::Failed => {
                Self::other(error.to_string())
            }
        }
    }
}

#[async_trait]
impl TypedToolOperation for ConfigAdminOperation {
    type Args = ConfigOperationArgs;
    type Output = ConfigOperationOutput;
    type Error = ConfigOperationError;
    fn metadata(&self, args: &Self::Args) -> ToolOperationMetadata {
        match args {
            ConfigOperationArgs::ConfigGet { .. } => metadata(
                "haven_config",
                "config_get",
                RiskLevel::Low,
                OperationIdempotency::Idempotent,
                ToolConcurrency::SharedResource("config".into()),
            ),
            ConfigOperationArgs::LogsLevel { .. } => metadata(
                "haven_config",
                "logs_level",
                RiskLevel::Medium,
                OperationIdempotency::Idempotent,
                ToolConcurrency::Resource("config".into()),
            ),
        }
    }
    fn default_metadata(&self) -> ToolOperationMetadata {
        metadata(
            "haven_config",
            "logs_level",
            RiskLevel::High,
            OperationIdempotency::Unknown,
            ToolConcurrency::Exclusive,
        )
    }
    fn input_schema(&self) -> Value {
        output_schema(vec![
            branch(
                "config_get",
                serde_json::json!({"path": {"type": "string"}}),
                &["operation"],
            ),
            branch(
                "logs_level",
                serde_json::json!({"level": {"type": "string", "enum": ["trace", "debug", "info", "warn", "error"]}}),
                &["operation", "level"],
            ),
        ])
    }
    fn error_metadata(&self, error: &Self::Error) -> ToolErrorMetadata {
        match error {
            ConfigOperationError::Cancelled => ToolErrorMetadata {
                class: crate::ToolErrorClass::UnknownOutcome,
                outcome: ToolExecutionOutcome::Cancelled,
                retryability: crate::ToolRetryability::Unknown,
            },
            ConfigOperationError::PathNotFound { .. } => ToolErrorMetadata::validation(),
            ConfigOperationError::SideEffectMayHaveHappened => ToolErrorMetadata {
                class: crate::ToolErrorClass::SideEffectMayHaveHappened,
                outcome: ToolExecutionOutcome::Failed,
                retryability: crate::ToolRetryability::Unknown,
            },
            ConfigOperationError::Unavailable | ConfigOperationError::Failed => {
                ToolErrorMetadata::other()
            }
        }
    }
    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        if cancel.is_cancelled() {
            return Err(ConfigOperationError::Cancelled);
        }
        if self.services.context.config_service.is_none() {
            return Err(ConfigOperationError::Unavailable);
        }
        match args {
            ConfigOperationArgs::ConfigGet { path } => {
                let value = self
                    .services
                    .config_get(path.as_deref())
                    .await
                    .map_err(|error| {
                        if let Some(path) = path.filter(|path| !path.is_empty()) {
                            let _ = error;
                            ConfigOperationError::PathNotFound { path }
                        } else {
                            ConfigOperationError::Failed
                        }
                    })?;
                Ok(ConfigOperationOutput::Config(ConfigViewOutput { value }))
            }
            ConfigOperationArgs::LogsLevel { level } => {
                let result = self
                    .services
                    .logs_level(level.clone())
                    .await
                    .map_err(|_| ConfigOperationError::SideEffectMayHaveHappened)?;
                Ok(ConfigOperationOutput::LogsLevel(LogLevelOutput {
                    level: result.level,
                    saved: result.saved,
                    version: result.version,
                }))
            }
        }
    }
}

pub type ConfigAdminTool = TypedToolAdapter<ConfigAdminOperation>;

pub fn new_config_admin_tool(context: ConfigAdminContext) -> ConfigAdminTool {
    TypedToolAdapter::new(
        "haven_config",
        "Read masked configuration or change the typed runtime log level.",
        ConfigAdminOperation::new(context),
    )
}

/// The current catalog generation's five typed admin operations. Native app
/// commands use this object as their structured entry; provider calls use the
/// five adapters returned by `tools()`.
#[derive(Clone)]
pub struct AdminSurfaces {
    pub(crate) diagnostics: DiagnosticsAdminOperation,
    pub(crate) config: ConfigAdminOperation,
    pub(crate) skills: SkillsAdminOperation,
    pub(crate) tools: ToolsAdminOperation,
    pub(crate) mcp: McpAdminOperation,
}

impl AdminSurfaces {
    pub(crate) fn new(
        context: AdminContext,
        skills_engine: SkillsEngine,
        mcp_manager: Arc<McpManager>,
        server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
        registry: ToolRegistry,
        max_instructions_bytes: usize,
        max_script_bytes: usize,
    ) -> Self {
        let services = Arc::new(AdminServices::new(
            context,
            skills_engine,
            mcp_manager,
            server_configs,
            registry,
            max_instructions_bytes,
            max_script_bytes,
        ));
        Self {
            diagnostics: DiagnosticsAdminOperation::new(services.clone()),
            config: ConfigAdminOperation::from_services(services.clone()),
            skills: SkillsAdminOperation::new(services.clone()),
            tools: ToolsAdminOperation::new(services.clone()),
            mcp: McpAdminOperation::new(services),
        }
    }

    pub(crate) fn tools(&self) -> Vec<ToolBox> {
        vec![
            Arc::new(TypedToolAdapter::new(
                AdminCapability::Diagnostics.name(),
                AdminCapability::Diagnostics.description(),
                self.diagnostics.clone(),
            )),
            Arc::new(TypedToolAdapter::new(
                AdminCapability::Config.name(),
                AdminCapability::Config.description(),
                self.config.clone(),
            )),
            Arc::new(TypedToolAdapter::new(
                AdminCapability::Skills.name(),
                AdminCapability::Skills.description(),
                self.skills.clone(),
            )),
            Arc::new(TypedToolAdapter::new(
                AdminCapability::Tools.name(),
                AdminCapability::Tools.description(),
                self.tools.clone(),
            )),
            Arc::new(TypedToolAdapter::new(
                AdminCapability::Mcp.name(),
                AdminCapability::Mcp.description(),
                self.mcp.clone(),
            )),
        ]
    }

    pub fn metadata(&self, request: &AdminRequest) -> ToolOperationMetadata {
        match request {
            AdminRequest::Diagnostics(args) => self.diagnostics.metadata(args),
            AdminRequest::Config(args) => self.config.metadata(args),
            AdminRequest::Skills(args) => self.skills.metadata(args),
            AdminRequest::Tools(args) => self.tools.metadata(args),
            AdminRequest::Mcp(args) => self.mcp.metadata(args),
            AdminRequest::NativeMcp(args) => self.mcp.native_metadata(args),
        }
    }

    pub async fn execute(
        &self,
        request: AdminRequest,
        cancel: CancellationToken,
    ) -> Result<ToolResult, AdminOperationError> {
        match request {
            AdminRequest::Diagnostics(args) => {
                let output = self.diagnostics.execute_typed(args, cancel).await?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::Config(args) => {
                let output = self
                    .config
                    .execute_typed(args, cancel)
                    .await
                    .map_err(AdminOperationError::from)?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::Skills(args) => {
                let output = self.skills.execute_typed(args, cancel).await?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::Tools(args) => {
                let output = self.tools.execute_typed(args, cancel).await?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::Mcp(args) => {
                let output = self.mcp.execute_typed(args, cancel).await?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::NativeMcp(args) => {
                let output = self.mcp.execute_native(args, cancel).await?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StructuredToolError, Tool, ToolsManager};
    use haven_common::SessionStatus;
    use haven_common::config::{ConfigLoader, ConfigPatch, ConfigService, InMemoryCredentialStore};
    use haven_memory::{Database, MemoryFactStore, SessionStore};
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn test_config_service(loader: ConfigLoader) -> ConfigService {
        ConfigService::new_with_credential_store(
            loader,
            Arc::new(InMemoryCredentialStore::default()),
        )
        .unwrap()
    }

    struct AdminOperationCase {
        surface: &'static str,
        operation: &'static str,
        input: Value,
        required: &'static [&'static str],
        idempotency: OperationIdempotency,
    }

    fn admin_operation_cases() -> Vec<AdminOperationCase> {
        vec![
            AdminOperationCase {
                surface: "haven_diagnostics",
                operation: "status",
                input: json!({"operation": "status"}),
                required: &["operation"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_diagnostics",
                operation: "logs_tail",
                input: json!({"operation": "logs_tail", "limit": 2}),
                required: &["operation"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_diagnostics",
                operation: "sessions",
                input: json!({"operation": "sessions", "limit": 1}),
                required: &["operation"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_diagnostics",
                operation: "errors",
                input: json!({"operation": "errors", "limit": 1}),
                required: &["operation"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_config",
                operation: "config_get",
                input: json!({"operation": "config_get", "path": "log.level"}),
                required: &["operation"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_config",
                operation: "logs_level",
                input: json!({"operation": "logs_level", "level": "debug"}),
                required: &["operation", "level"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_skills",
                operation: "skills_list",
                input: json!({"operation": "skills_list"}),
                required: &["operation"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_skills",
                operation: "skill_enable",
                input: json!({"operation": "skill_enable", "name": "demo"}),
                required: &["operation", "name"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_skills",
                operation: "skill_disable",
                input: json!({"operation": "skill_disable", "name": "demo"}),
                required: &["operation", "name"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_skills",
                operation: "skill_create",
                input: json!({
                    "operation": "skill_create",
                    "name": "demo",
                    "description": "Demo skill",
                    "instructions": "Do the demo",
                    "script": "print('{}')",
                }),
                required: &["operation", "name", "description", "instructions", "script"],
                idempotency: OperationIdempotency::Unknown,
            },
            AdminOperationCase {
                surface: "haven_tools",
                operation: "tool_enable",
                input: json!({"operation": "tool_enable", "name": "shell"}),
                required: &["operation", "name"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_tools",
                operation: "tool_disable",
                input: json!({"operation": "tool_disable", "name": "shell"}),
                required: &["operation", "name"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_list",
                input: json!({"operation": "mcp_list"}),
                required: &["operation"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_connect",
                input: json!({"operation": "mcp_connect", "name": "demo"}),
                required: &["operation", "name"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_disconnect",
                input: json!({"operation": "mcp_disconnect", "name": "demo"}),
                required: &["operation", "name"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_add_stdio",
                input: json!({
                    "operation": "mcp_add",
                    "name": "demo",
                    "transport": "stdio",
                    "command": "demo-mcp",
                    "enabled": false,
                    "auto_connect": false,
                }),
                required: &["operation", "name", "transport", "command"],
                idempotency: OperationIdempotency::Unknown,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_add_http",
                input: json!({
                    "operation": "mcp_add",
                    "name": "demo-http",
                    "transport": "http",
                    "url": "https://example.invalid/mcp",
                    "enabled": false,
                    "auto_connect": false,
                }),
                required: &["operation", "name", "transport", "url"],
                idempotency: OperationIdempotency::Unknown,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_update",
                input: json!({"operation": "mcp_update", "name": "demo", "enabled": false}),
                required: &["operation", "name"],
                idempotency: OperationIdempotency::Unknown,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_toggle",
                input: json!({"operation": "mcp_toggle", "name": "demo", "enabled": false}),
                required: &["operation", "name", "enabled"],
                idempotency: OperationIdempotency::Idempotent,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_remove",
                input: json!({"operation": "mcp_remove", "name": "demo"}),
                required: &["operation", "name"],
                idempotency: OperationIdempotency::Unknown,
            },
            AdminOperationCase {
                surface: "haven_mcp",
                operation: "mcp_reload",
                input: json!({"operation": "mcp_reload"}),
                required: &["operation"],
                idempotency: OperationIdempotency::Idempotent,
            },
        ]
    }

    fn tool_for<'a>(tools: &'a [ToolBox], name: &str) -> &'a dyn Tool {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("missing admin surface {name}"))
            .as_ref()
    }

    fn remove_field(input: &Value, field: &str) -> Value {
        let mut input = input
            .as_object()
            .cloned()
            .expect("admin operation input is an object");
        input.remove(field);
        Value::Object(input)
    }

    async fn assert_validation_rejection(tool: &dyn Tool, input: Value, label: &str) {
        assert!(
            tool.validate_input(&input).is_err(),
            "{label}: schema accepted {input}"
        );
        let error = tool
            .execute(input, CancellationToken::new())
            .await
            .expect_err(label);
        let structured = error
            .downcast_ref::<StructuredToolError>()
            .unwrap_or_else(|| panic!("{label}: error lost structured metadata: {error}"));
        assert_eq!(
            structured.metadata().class,
            crate::ToolErrorClass::Validation,
            "{label}: wrong error class"
        );
    }

    fn test_surfaces() -> (AdminSurfaces, TempDir) {
        test_surfaces_with_stores(None, None)
    }

    fn test_surfaces_with_db(db: Arc<Database>) -> (AdminSurfaces, TempDir) {
        test_surfaces_with_stores(
            Some(SessionStore::new(db.clone())),
            Some(MemoryFactStore::new(db)),
        )
    }

    fn test_surfaces_with_stores(
        session_store: Option<SessionStore>,
        memory_facts: Option<MemoryFactStore>,
    ) -> (AdminSurfaces, TempDir) {
        let dir = TempDir::new().expect("temporary config directory");
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let context = AdminContext {
            config_service: Some(Arc::new(test_config_service(loader))),
            config_apply_gate: Some(Arc::new(tokio::sync::Mutex::new(()))),
            session_store,
            memory_facts,
            router: None,
            log_path: Some(dir.path().join("logs").join("haven.log")),
            file_logging_enabled: true,
            log_level: None,
            tool_control: None,
        };
        (
            AdminSurfaces::new(
                context,
                SkillsEngine::new(),
                Arc::new(McpManager::new()),
                Arc::new(RwLock::new(HashMap::new())),
                ToolRegistry::new(),
                256 * 1024,
                512 * 1024,
            ),
            dir,
        )
    }

    fn config_tool() -> (ConfigAdminTool, Arc<ConfigService>, TempDir) {
        let dir = TempDir::new().expect("temporary config directory");
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(test_config_service(loader));
        let tool = new_config_admin_tool(ConfigAdminContext {
            config_service: Some(service.clone()),
            config_apply_gate: None,
            log_level: None,
        });
        (tool, service, dir)
    }

    #[test]
    fn five_surfaces_are_typed_and_have_disjoint_names() {
        let (surfaces, _dir) = test_surfaces();
        let tools = surfaces.tools();
        assert_eq!(tools.len(), 5);
        let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();
        assert_eq!(
            names,
            vec![
                "haven_diagnostics",
                "haven_config",
                "haven_skills",
                "haven_tools",
                "haven_mcp"
            ]
        );
        for tool in tools {
            assert!(tool.input_schema()["oneOf"].is_array());
            assert_eq!(
                tool.risk_level(&json!({})),
                RiskLevel::High,
                "malformed input must use a conservative risk"
            );
        }
    }

    fn model_admin_operation_names(surfaces: &AdminSurfaces) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for tool in surfaces.tools() {
            let tool_name = tool.name();
            let namespace = match tool_name.as_str() {
                "haven_diagnostics" => "haven.diagnostics",
                "haven_config" => "haven.config",
                "haven_skills" => "haven.skills",
                "haven_tools" => "haven.tools",
                "haven_mcp" => "haven.mcp",
                other => panic!("unexpected Admin tool {other}"),
            };
            let schema = tool.input_schema();
            let branches = schema["oneOf"]
                .as_array()
                .unwrap_or_else(|| panic!("{tool_name} is missing operation branches"));
            for branch in branches {
                let operation = branch["properties"]["operation"]["const"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{tool_name} has an unnamed operation"));
                names.insert(format!("{namespace}.{operation}"));
            }
        }
        names
    }

    fn typed_request_for_case(case: &AdminOperationCase) -> AdminRequest {
        match case.surface {
            "haven_diagnostics" => AdminRequest::Diagnostics(
                serde_json::from_value(case.input.clone()).expect("valid diagnostics fixture"),
            ),
            "haven_config" => AdminRequest::Config(
                serde_json::from_value(case.input.clone()).expect("valid config fixture"),
            ),
            "haven_skills" => AdminRequest::Skills(
                serde_json::from_value(case.input.clone()).expect("valid skills fixture"),
            ),
            "haven_tools" => AdminRequest::Tools(
                serde_json::from_value(case.input.clone()).expect("valid tools fixture"),
            ),
            "haven_mcp" => AdminRequest::Mcp(
                serde_json::from_value(case.input.clone()).expect("valid MCP fixture"),
            ),
            other => panic!("unexpected Admin surface {other}"),
        }
    }

    #[test]
    fn model_and_native_admin_risk_levels_match_for_every_shared_operation() {
        let (surfaces, _dir) = test_surfaces();
        let cases = admin_operation_cases();
        let requests: Vec<_> = cases.iter().map(typed_request_for_case).collect();
        let request_names: BTreeSet<_> = requests
            .iter()
            .map(|request| request.model_operation_name().to_owned())
            .collect();
        assert_eq!(
            request_names.len(),
            20,
            "update parity cases when Admin changes"
        );
        assert_eq!(
            request_names,
            model_admin_operation_names(&surfaces),
            "native typed requests must cover every model-visible Admin operation"
        );

        let tools = surfaces.tools();
        for (case, request) in cases.iter().zip(&requests) {
            let operation = request.model_operation_name();
            let model_tool = tool_for(&tools, request.tool_name());
            let model_risk = model_tool.risk_level(&case.input);
            let native_risk = surfaces.metadata(request).risk_level;
            let contract_risk = super::super::operation_contract::operation_contract(operation)
                .risk_override
                .unwrap_or_else(|| panic!("{operation} must declare an explicit risk level"));

            assert_eq!(
                model_risk, contract_risk,
                "model risk drift for {operation}"
            );
            assert_eq!(
                native_risk, contract_risk,
                "native risk drift for {operation}"
            );
        }
    }

    #[test]
    fn renderer_mcp_operations_use_typed_metadata_without_entering_model_schema() {
        let (surfaces, _dir) = test_surfaces();
        let tools = surfaces.tools();
        let mcp_tool = tool_for(&tools, "haven_mcp");
        let mut reconcile = McpReconcile::default();
        reconcile.to_connect_new.push(McpServerConfig {
            name: "new-server".into(),
            transport: McpTransportType::Stdio,
            command: "private-command".into(),
            args: vec!["private-arg".into()],
            env: vec!["TOKEN=private-value".into()],
            env_refs: vec![],
            cwd: None,
            url: String::new(),
            enabled: true,
        });
        reconcile.to_connect_changed.push(McpServerConfig {
            name: "changed-server".into(),
            transport: McpTransportType::Stdio,
            command: "private-command".into(),
            args: vec![],
            env: vec![],
            env_refs: vec![],
            cwd: None,
            url: String::new(),
            enabled: true,
        });
        reconcile.to_remove.push("removed-server".into());
        let plan = McpRefreshPlan::from_reconcile(9, &reconcile);
        assert_eq!(
            plan.targets,
            vec![
                McpRefreshTarget {
                    name: "changed-server".into(),
                    action: McpRefreshAction::Reconnect,
                },
                McpRefreshTarget {
                    name: "new-server".into(),
                    action: McpRefreshAction::Connect,
                },
                McpRefreshTarget {
                    name: "removed-server".into(),
                    action: McpRefreshAction::Disconnect,
                },
            ]
        );
        let encoded_plan = serde_json::to_string(&plan).unwrap();
        assert!(!encoded_plan.contains("private-command"));
        assert!(!encoded_plan.contains("private-arg"));
        assert!(!encoded_plan.contains("private-value"));

        let reconnect = AdminRequest::NativeMcp(NativeMcpOperationArgs::McpReconnect {
            name: "one-server".into(),
            config_version: 9,
        });
        let reconnect_metadata = surfaces.metadata(&reconnect);
        assert_eq!(reconnect.model_operation_name(), "haven.mcp.mcp_reconnect");
        assert_eq!(reconnect_metadata.risk_level, RiskLevel::Medium);
        assert_eq!(reconnect.network_access(), crate::NetworkAccess::Opaque);

        let refresh = AdminRequest::NativeMcp(NativeMcpOperationArgs::McpRefresh { plan });
        let refresh_metadata = surfaces.metadata(&refresh);
        assert_eq!(refresh.model_operation_name(), "haven.mcp.mcp_refresh");
        assert_eq!(refresh_metadata.risk_level, RiskLevel::Medium);
        assert_eq!(refresh.network_access(), crate::NetworkAccess::Opaque);

        let disconnect_only = AdminRequest::NativeMcp(NativeMcpOperationArgs::McpRefresh {
            plan: McpRefreshPlan {
                config_version: 9,
                targets: vec![McpRefreshTarget {
                    name: "removed-server".into(),
                    action: McpRefreshAction::Disconnect,
                }],
            },
        });
        assert_eq!(disconnect_only.network_access(), crate::NetworkAccess::None);

        for request in [reconnect, refresh] {
            let input = request.input();
            assert!(mcp_tool.validate_input(&input).is_err());
            assert!(
                mcp_tool.input_schema()["oneOf"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(
                        |branch| branch["properties"]["operation"]["const"] != "mcp_reconnect"
                            && branch["properties"]["operation"]["const"] != "mcp_refresh"
                    )
            );
        }
    }

    #[tokio::test]
    async fn native_mcp_connection_requests_are_asked_or_blocked_before_execution() {
        let (surfaces, _dir) = test_surfaces();
        let requests = [
            AdminRequest::NativeMcp(NativeMcpOperationArgs::McpReconnect {
                name: "server-a".into(),
                config_version: 3,
            }),
            AdminRequest::NativeMcp(NativeMcpOperationArgs::McpRefresh {
                plan: McpRefreshPlan {
                    config_version: 3,
                    targets: vec![McpRefreshTarget {
                        name: "server-a".into(),
                        action: McpRefreshAction::Connect,
                    }],
                },
            }),
        ];

        for request in requests {
            let metadata = surfaces.metadata(&request);
            let tool_name = request.model_operation_name();
            let authorization_request = crate::AuthorizationRequest::new(
                None,
                tool_name,
                request.input(),
                crate::OperationPolicy::native(
                    tool_name,
                    tool_name.into(),
                    metadata.risk_level,
                    request.network_access(),
                ),
            );
            let engine = crate::AuthorizationEngine::new();
            assert!(matches!(
                engine.authorize(&authorization_request).await,
                crate::AuthorizationDecision::RequiresConfirmation { .. }
            ));

            engine
                .set_boundaries(
                    haven_common::types::SandboxMode::FullAccess,
                    Vec::new(),
                    haven_common::types::NetworkPolicy::Deny,
                )
                .await;
            assert!(matches!(
                engine.authorize(&authorization_request).await,
                crate::AuthorizationDecision::Blocked {
                    reason_code: crate::AuthorizationReasonCode::NetworkPolicy,
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn stale_native_mcp_refresh_plan_fails_before_connection_side_effects() {
        let (surfaces, _dir) = test_surfaces();
        let manager = surfaces.mcp.services.mcp_manager.clone();
        let request = AdminRequest::NativeMcp(NativeMcpOperationArgs::McpRefresh {
            plan: McpRefreshPlan {
                config_version: 1,
                targets: vec![McpRefreshTarget {
                    name: "must-not-connect".into(),
                    action: McpRefreshAction::Connect,
                }],
            },
        });

        let error = surfaces
            .execute(request, CancellationToken::new())
            .await
            .expect_err("stale plan must be rejected before execution");
        assert!(error.to_string().contains("authorization is stale"));
        assert_eq!(
            operation_error_metadata(&error).class,
            crate::ToolErrorClass::Validation
        );
        assert!(manager.list_clients().await.is_empty());

        let current_version = surfaces
            .mcp
            .services
            .context
            .config_service
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap()
            .version;
        let stale_targets = AdminRequest::NativeMcp(NativeMcpOperationArgs::McpRefresh {
            plan: McpRefreshPlan {
                config_version: current_version,
                targets: vec![McpRefreshTarget {
                    name: "must-not-connect".into(),
                    action: McpRefreshAction::Connect,
                }],
            },
        });
        let error = surfaces
            .execute(stale_targets, CancellationToken::new())
            .await
            .expect_err("a changed target set must be rejected before effects");
        assert!(
            error
                .to_string()
                .contains("targets changed after authorization")
        );
        assert_eq!(
            operation_error_metadata(&error).class,
            crate::ToolErrorClass::Validation
        );
        assert!(manager.list_clients().await.is_empty());
    }

    #[tokio::test]
    async fn diagnostics_session_operations_report_unavailable_without_session_store() {
        let (surfaces, _dir) = test_surfaces();

        let status = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Status),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(status.output["sessions"], json!({"unavailable": true}));

        for args in [
            DiagnosticsOperationArgs::Sessions { limit: None },
            DiagnosticsOperationArgs::Errors { limit: None },
        ] {
            let result = surfaces
                .execute(AdminRequest::Diagnostics(args), CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(result.output, json!({"unavailable": true}));
        }
    }

    #[tokio::test]
    async fn diagnostics_status_counts_all_sessions_and_only_groups_the_recent_fifty() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let older_error = db.create_session("older error").unwrap();
        db.update_session_status(&older_error.id, SessionStatus::Error)
            .unwrap();
        db.conn()
            .execute(
                "UPDATE sessions SET created_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                [older_error.id.as_str()],
            )
            .unwrap();
        db.cache_invalidate_sessions();

        for index in 0..50 {
            let session = db.create_session(&format!("recent {index}")).unwrap();
            db.update_session_status(&session.id, SessionStatus::Completed)
                .unwrap();
        }

        let (surfaces, _dir) = test_surfaces_with_db(db);
        let result = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Status),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(result.output["sessions"]["total"], 51);
        assert_eq!(
            result.output["sessions"]["recent_50_by_status"],
            json!({"completed": 50})
        );
    }

    #[tokio::test]
    async fn diagnostics_lists_keep_limit_order_and_error_filter_semantics() {
        fn set_created_at(db: &Database, session_id: &str, created_at: &str) {
            db.conn()
                .execute(
                    "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                    [created_at, session_id],
                )
                .unwrap();
            db.cache_invalidate_sessions();
        }

        let db = Arc::new(Database::open_in_memory().unwrap());
        let oldest_error = db.create_session("oldest error").unwrap();
        db.update_session_status(&oldest_error.id, SessionStatus::Error)
            .unwrap();
        set_created_at(&db, &oldest_error.id, "2020-01-01T00:00:00Z");

        let middle_error = db.create_session("middle error").unwrap();
        db.update_session_status(&middle_error.id, SessionStatus::Error)
            .unwrap();
        set_created_at(&db, &middle_error.id, "2021-01-01T00:00:00Z");

        let recent_completed = db.create_session("completed").unwrap();
        db.update_session_status(&recent_completed.id, SessionStatus::Completed)
            .unwrap();
        set_created_at(&db, &recent_completed.id, "2022-01-01T00:00:00Z");

        let newest_error = db.create_session("你 🙂").unwrap();
        db.update_session_status(&newest_error.id, SessionStatus::Error)
            .unwrap();
        set_created_at(&db, &newest_error.id, "2023-01-01T00:00:00Z");

        let (surfaces, _dir) = test_surfaces_with_db(db);
        let sessions = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Sessions { limit: Some(2) }),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(sessions["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(sessions["sessions"][0]["id"], newest_error.id);
        assert_eq!(sessions["sessions"][1]["id"], recent_completed.id);
        assert_eq!(sessions["sessions"][0]["input_chars"], 3);

        let clamped_sessions = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Sessions { limit: Some(0) }),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(clamped_sessions["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(clamped_sessions["sessions"][0]["id"], newest_error.id);

        let errors = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Errors { limit: Some(2) }),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(errors["errors"].as_array().unwrap().len(), 1);
        assert_eq!(errors["errors"][0]["id"], newest_error.id);
    }

    #[tokio::test]
    async fn admin_read_projections_keep_exact_wire_shapes_and_filter_private_content() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let transcript = "private-transcript-marker";
        let session = db.create_session(transcript).unwrap();
        db.update_session_title(&session.id, "safe title").unwrap();
        db.update_session_status(&session.id, SessionStatus::Error)
            .unwrap();
        db.conn()
            .execute(
                "UPDATE sessions SET created_at = '2024-01-02T03:04:05Z', updated_at = '2024-01-03T04:05:06Z' WHERE id = ?1",
                [session.id.as_str()],
            )
            .unwrap();
        db.cache_invalidate_sessions();

        let (surfaces, dir) = test_surfaces_with_db(db);
        let skill_root = dir.path().join("skills");
        let skill_dir = skill_root.join("sample");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Skill: sample\n\n## Metadata\n- name: sample\n- description: safe description\n\n## Instructions\nPrivate instructions are not part of this output.\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("scripts").join("main.py"), "print('{}')\n").unwrap();
        surfaces
            .skills
            .services
            .skills_engine
            .set_config(Some(skill_root), None)
            .await
            .unwrap();

        surfaces.mcp.services.server_configs.write().await.insert(
            "private-server".into(),
            McpServerConfig {
                name: "private-server".into(),
                transport: McpTransportType::Stdio,
                command: "hidden-command".into(),
                args: vec!["PRIVATE_ARG_MARKER".into()],
                env: vec!["TOKEN=PRIVATE_ENV_MARKER".into()],
                env_refs: vec![],
                cwd: None,
                url: String::new(),
                enabled: true,
            },
        );

        let session_wire = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Sessions { limit: None }),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(
            session_wire,
            json!({
                "sessions": [{
                    "id": session.id,
                    "status": "error",
                    "title": "safe title",
                    "input_chars": transcript.chars().count(),
                    "created_at": "2024-01-02T03:04:05Z",
                    "updated_at": "2024-01-03T04:05:06Z",
                }]
            })
        );
        let session_text = serde_json::to_string(&session_wire).unwrap();
        assert!(!session_text.contains(transcript));

        let errors_wire = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Errors { limit: None }),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(
            errors_wire,
            json!({
                "errors": [{
                    "id": session.id,
                    "title": "safe title",
                    "input_chars": transcript.chars().count(),
                    "created_at": "2024-01-02T03:04:05Z",
                }]
            })
        );
        assert!(
            !serde_json::to_string(&errors_wire)
                .unwrap()
                .contains(transcript)
        );

        let skills_wire = surfaces
            .execute(
                AdminRequest::Skills(SkillsOperationArgs::SkillsList),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(
            skills_wire,
            json!({
                "skills": [{
                    "name": "sample",
                    "enabled": true,
                    "description": "safe description",
                    "root": skill_dir.to_string_lossy(),
                }]
            })
        );
        assert!(
            !serde_json::to_string(&skills_wire)
                .unwrap()
                .contains("Private instructions")
        );

        let mcp_wire = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpList),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(
            mcp_wire,
            json!([{
                "name": "private-server",
                "enabled": true,
                "connected": false,
                "tools": 0,
                "last_error": "",
                "diagnostic": null,
            }])
        );
        let mcp_text = serde_json::to_string(&mcp_wire).unwrap();
        assert!(!mcp_text.contains("PRIVATE_ARG_MARKER"));
        assert!(!mcp_text.contains("PRIVATE_ENV_MARKER"));
        assert!(!mcp_text.contains("hidden-command"));

        let log_path = dir.path().join("logs").join("haven.2026-10-03");
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        std::fs::write(
            &log_path,
            "safe line\napi_key=PRIVATE_LOG_MARKER\ntranscript: private words\nlast line\n",
        )
        .unwrap();
        let logs_wire = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::LogsTail { limit: Some(3) }),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .output;
        assert_eq!(
            logs_wire,
            json!({
                "path": log_path.to_string_lossy(),
                "total_lines": 4,
                "lines": [
                    "[redacted diagnostic line]",
                    "[redacted diagnostic line]",
                    "last line",
                ],
            })
        );
        assert!(
            !serde_json::to_string(&logs_wire)
                .unwrap()
                .contains("PRIVATE_LOG_MARKER")
        );
    }

    #[tokio::test]
    async fn admin_mutation_acks_keep_wire_shape_and_mcp_duplicate_semantics() {
        let (surfaces, dir) = test_surfaces();

        let level = surfaces
            .execute(
                AdminRequest::Config(ConfigOperationArgs::LogsLevel {
                    level: LogLevel::Warn,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            level.output,
            json!({"level": "warn", "saved": true, "version": 1})
        );

        let tool = surfaces
            .execute(
                AdminRequest::Tools(ToolsOperationArgs::ToolDisable {
                    name: "shell".into(),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            tool.output,
            json!({
                "name": "shell",
                "enabled": false,
                "saved": true,
                "note": "take effect immediately",
            })
        );

        let skills_root = dir.path().join("skills");
        std::fs::create_dir_all(skills_root.join("demo")).unwrap();
        std::fs::write(
            skills_root.join("demo").join("SKILL.md"),
            "# Skill: demo\n\n## Metadata\n- name: demo\n- description: demo skill\n\n## Instructions\nDo the demo.\n",
        )
        .unwrap();
        surfaces
            .skills
            .services
            .skills_engine
            .set_config(Some(skills_root.clone()), None)
            .await
            .unwrap();
        let skill_set = surfaces
            .execute(
                AdminRequest::Skills(SkillsOperationArgs::SkillDisable {
                    name: "demo".into(),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            skill_set.output,
            json!({
                "name": "demo",
                "enabled": false,
                "saved": true,
                "note": "take effect immediately for new loads",
            })
        );

        let skill_create = surfaces
            .execute(
                AdminRequest::Skills(SkillsOperationArgs::SkillCreate {
                    name: "created".into(),
                    description: "created skill".into(),
                    instructions: "Do something useful.".into(),
                    language: None,
                    version: None,
                    script: Some("print('not returned')".into()),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            skill_create.output,
            json!({
                "name": "created",
                "created": true,
                "root": skills_root.join("created").to_string_lossy(),
                "has_script": true,
            })
        );
        assert!(
            !serde_json::to_string(&skill_create.output)
                .unwrap()
                .contains("print('not returned')")
        );

        let reserved_skill_create = surfaces
            .execute(
                AdminRequest::Skills(SkillsOperationArgs::SkillCreate {
                    name: "CON".into(),
                    description: "reserved name".into(),
                    instructions: "Do something useful.".into(),
                    language: None,
                    version: None,
                    script: Some("print('not created')".into()),
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(reserved_skill_create.is_err());
        assert!(
            surfaces
                .skills
                .services
                .skills_engine
                .list()
                .await
                .iter()
                .all(|skill| skill.name != "CON")
        );

        let initial = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpAdd {
                    name: "duplicate-server".into(),
                    transport: McpTransportType::Stdio,
                    command: Some("server-one".into()),
                    url: None,
                    args: vec!["PRIVATE_ARG_MARKER".into()],
                    env: vec!["TOKEN=PRIVATE_ENV_MARKER".into()],
                    cwd: None,
                    enabled: false,
                    auto_connect: false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            initial.output,
            json!({
                "name": "duplicate-server",
                "enabled": false,
                "saved": true,
                "connected": false,
            })
        );
        let duplicate = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpAdd {
                    name: "duplicate-server".into(),
                    transport: McpTransportType::Stdio,
                    command: Some("server-two".into()),
                    url: None,
                    args: vec!["DUPLICATE_PRIVATE_ARG".into()],
                    env: vec!["TOKEN=DUPLICATE_PRIVATE_ENV".into()],
                    cwd: None,
                    enabled: false,
                    auto_connect: false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            duplicate.output,
            json!({
                "name": "duplicate-server",
                "enabled": false,
                "saved": true,
                "connected": false,
            })
        );
        assert_eq!(
            surfaces
                .mcp
                .services
                .context
                .config_service
                .as_ref()
                .unwrap()
                .snapshot()
                .unwrap()
                .config
                .mcp_servers
                .iter()
                .filter(|server| server.name == "duplicate-server")
                .count(),
            1
        );
        assert_eq!(
            surfaces
                .mcp
                .services
                .context
                .config_service
                .as_ref()
                .unwrap()
                .snapshot()
                .unwrap()
                .config
                .mcp_servers[0]
                .command,
            "server-two"
        );
        let duplicate_text = serde_json::to_string(&duplicate.output).unwrap();
        assert!(!duplicate_text.contains("PRIVATE_ARG_MARKER"));
        assert!(!duplicate_text.contains("PRIVATE_ENV_MARKER"));
        assert!(!duplicate_text.contains("DUPLICATE_PRIVATE_ARG"));
        assert!(!duplicate_text.contains("DUPLICATE_PRIVATE_ENV"));

        let updated = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpUpdate {
                    name: "duplicate-server".into(),
                    transport: None,
                    command: None,
                    url: None,
                    args: None,
                    env: None,
                    cwd: None,
                    enabled: Some(false),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            updated.output,
            json!({
                "name": "duplicate-server",
                "enabled": false,
                "saved": true,
                "connected": false,
            })
        );
        let toggled = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpToggle {
                    name: "duplicate-server".into(),
                    enabled: false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            toggled.output,
            json!({
                "name": "duplicate-server",
                "enabled": false,
                "saved": true,
                "connected": false,
            })
        );
        let disconnected = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpDisconnect {
                    name: "duplicate-server".into(),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            disconnected.output,
            json!({"name": "duplicate-server", "connected": false})
        );
        let removed = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpRemove {
                    name: "duplicate-server".into(),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            removed.output,
            json!({"name": "duplicate-server", "removed": true, "connected": false})
        );

        let missing_command = dir
            .path()
            .join("missing-mcp-command.exe")
            .to_string_lossy()
            .to_string();
        let partial_add = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpAdd {
                    name: "partial-server".into(),
                    transport: McpTransportType::Stdio,
                    command: Some(missing_command.clone()),
                    url: None,
                    args: vec!["PARTIAL_PRIVATE_ARG".into()],
                    env: vec!["TOKEN=PARTIAL_PRIVATE_ENV".into()],
                    cwd: None,
                    enabled: true,
                    auto_connect: true,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(partial_add.output["name"], json!("partial-server"));
        assert_eq!(partial_add.output["enabled"], json!(true));
        assert_eq!(partial_add.output["saved"], json!(true));
        assert_eq!(partial_add.output["connected"], json!(false));
        assert!(
            partial_add.output["warning"]
                .as_str()
                .is_some_and(|w| !w.is_empty())
        );
        assert_eq!(partial_add.output.as_object().unwrap().len(), 5);
        let partial_text = serde_json::to_string(&partial_add.output).unwrap();
        assert!(!partial_text.contains(&missing_command));
        assert!(!partial_text.contains("PARTIAL_PRIVATE_ARG"));
        assert!(!partial_text.contains("PARTIAL_PRIVATE_ENV"));
    }

    #[tokio::test]
    async fn admin_empty_and_unavailable_projections_keep_distinct_wire_shapes() {
        let empty_db = Arc::new(Database::open_in_memory().unwrap());
        let (surfaces, dir) = test_surfaces_with_db(empty_db);

        let sessions = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Sessions { limit: None }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(sessions.output, json!({"sessions": []}));
        let errors = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::Errors { limit: None }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(errors.output, json!({"errors": []}));

        let skills = surfaces
            .execute(
                AdminRequest::Skills(SkillsOperationArgs::SkillsList),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(skills.output, json!({"skills": []}));
        let mcp = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpList),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(mcp.output, json!([]));

        let configured_log_path = dir.path().join("logs").join("haven.log");
        let current_log_path = dir.path().join("logs").join("haven.2026-10-03");
        std::fs::create_dir_all(current_log_path.parent().unwrap()).unwrap();
        std::fs::write(&current_log_path, "").unwrap();
        let empty_logs = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::LogsTail { limit: None }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            empty_logs.output,
            json!({"path": current_log_path.to_string_lossy(), "total_lines": 0, "lines": []})
        );

        std::fs::remove_file(&current_log_path).unwrap();
        let missing_logs = surfaces
            .execute(
                AdminRequest::Diagnostics(DiagnosticsOperationArgs::LogsTail { limit: None }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let missing_logs = missing_logs.output;
        assert_eq!(
            missing_logs["path"],
            json!(configured_log_path.to_string_lossy())
        );
        assert_eq!(missing_logs.as_object().unwrap().len(), 2);
        assert!(
            missing_logs["error"]
                .as_str()
                .unwrap()
                .starts_with("no log file found yet")
        );
    }

    #[tokio::test]
    async fn mcp_reload_and_native_refresh_keep_partial_failure_shapes_private() {
        let (surfaces, dir) = test_surfaces();
        let command = dir
            .path()
            .join("does-not-exist-mcp-server.exe")
            .to_string_lossy()
            .to_string();
        let server = McpServerConfig {
            name: "broken-server".into(),
            transport: McpTransportType::Stdio,
            command: command.clone(),
            args: vec!["PRIVATE_MCP_ARG_MARKER".into()],
            env: vec!["TOKEN=PRIVATE_MCP_ENV_MARKER".into()],
            env_refs: vec![],
            cwd: None,
            url: String::new(),
            enabled: true,
        };
        let config_service = surfaces
            .mcp
            .services
            .context
            .config_service
            .as_ref()
            .unwrap();
        config_service
            .apply_patch(ConfigPatch::McpServers(vec![server]))
            .unwrap();

        let reload = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpReload),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(reload.output["reloaded"], json!(true));
        assert_eq!(reload.output["connected"].as_array().unwrap().len(), 1);
        let row = &reload.output["connected"][0];
        assert_eq!(row["name"], json!("broken-server"));
        assert_eq!(row["connected"], json!(false));
        assert!(row["error"].as_str().is_some_and(|error| !error.is_empty()));
        let mut row_keys: Vec<_> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        row_keys.sort_unstable();
        assert_eq!(row_keys, vec!["connected", "error", "name"]);
        let reload_text = serde_json::to_string(&reload.output).unwrap();
        assert!(!reload_text.contains(&command));
        assert!(!reload_text.contains("PRIVATE_MCP_ARG_MARKER"));
        assert!(!reload_text.contains("PRIVATE_MCP_ENV_MARKER"));

        let snapshot = config_service.snapshot().unwrap();
        let reconcile = surfaces
            .mcp
            .services
            .mcp_manager
            .reconcile_servers(&snapshot.config.mcp_servers)
            .await;
        let plan = McpRefreshPlan::from_reconcile(snapshot.version, &reconcile);
        assert_eq!(plan.targets.len(), 1);
        let refresh = surfaces
            .execute(
                AdminRequest::NativeMcp(NativeMcpOperationArgs::McpRefresh { plan }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            refresh.output,
            json!({
                "added": [],
                "removed": [],
                "updated": [],
                "failed": ["broken-server"],
            })
        );
        let refresh_text = serde_json::to_string(&refresh.output).unwrap();
        assert!(!refresh_text.contains(&command));
        assert!(!refresh_text.contains("PRIVATE_MCP_ARG_MARKER"));
        assert!(!refresh_text.contains("PRIVATE_MCP_ENV_MARKER"));

        let reconnect_error = surfaces
            .execute(
                AdminRequest::NativeMcp(NativeMcpOperationArgs::McpReconnect {
                    name: "broken-server".into(),
                    config_version: snapshot.version,
                }),
                CancellationToken::new(),
            )
            .await
            .expect_err("failed refresh does not create a reconnectable client");
        assert!(reconnect_error.to_string().contains("no longer connected"));
        assert!(
            !reconnect_error
                .to_string()
                .contains("PRIVATE_MCP_ENV_MARKER")
        );
    }

    #[test]
    fn admin_output_union_serializes_exact_untagged_acknowledgement_branches() {
        let connected = AdminOperationOutput::mcp_connection(McpConnectionOutput {
            name: "server".into(),
            connected: true,
        });
        assert_eq!(
            serde_json::to_value(connected).unwrap(),
            json!({"name": "server", "connected": true})
        );

        let added_without_warning = AdminOperationOutput::mcp_add(McpAddOutput {
            name: "server".into(),
            enabled: true,
            saved: true,
            connected: true,
            warning: None,
        });
        assert_eq!(
            serde_json::to_value(added_without_warning).unwrap(),
            json!({
                "name": "server",
                "enabled": true,
                "saved": true,
                "connected": true,
            })
        );

        let added_with_warning = AdminOperationOutput::mcp_add(McpAddOutput {
            name: "server".into(),
            enabled: true,
            saved: true,
            connected: false,
            warning: Some("config saved but connect failed: [redacted diagnostic line]".into()),
        });
        assert_eq!(
            serde_json::to_value(added_with_warning).unwrap(),
            json!({
                "name": "server",
                "enabled": true,
                "saved": true,
                "connected": false,
                "warning": "config saved but connect failed: [redacted diagnostic line]",
            })
        );

        let reload = AdminOperationOutput::mcp_reload(McpReloadOutput {
            reloaded: true,
            connected: vec![
                admin_services::McpReloadConnectionOutput::Connected {
                    name: "ok".into(),
                    connected: true,
                },
                admin_services::McpReloadConnectionOutput::Failed {
                    name: "failed".into(),
                    connected: false,
                    error: "[redacted diagnostic line]".into(),
                },
            ],
        });
        assert_eq!(
            serde_json::to_value(reload).unwrap(),
            json!({
                "reloaded": true,
                "connected": [
                    {"name": "ok", "connected": true},
                    {"name": "failed", "connected": false, "error": "[redacted diagnostic line]"},
                ],
            })
        );
    }

    #[test]
    fn native_request_input_matches_provider_shape() {
        let request = AdminRequest::Tools(ToolsOperationArgs::ToolDisable {
            name: "shell".into(),
        });
        assert_eq!(
            request.input(),
            json!({"operation": "tool_disable", "name": "shell"})
        );
    }

    #[tokio::test]
    async fn typed_surface_schemas_reject_cross_operation_fields() {
        let (surfaces, _dir) = test_surfaces();
        let tools = surfaces.tools();
        let diagnostics = &tools[0];
        assert!(
            diagnostics
                .validate_input(&json!({"operation": "status"}))
                .is_ok()
        );
        assert!(
            diagnostics
                .validate_input(&json!({"operation": "status", "limit": 2}))
                .is_err()
        );

        let skills = &tools[2];
        assert!(
            skills
                .validate_input(&json!({"operation": "skill_enable", "name": "demo"}))
                .is_ok()
        );
        assert!(
            skills
                .validate_input(&json!({"operation": "skill_enable", "instructions": "x"}))
                .is_err()
        );

        let mcp = &tools[4];
        assert!(
            mcp.validate_input(&json!({
                "operation": "mcp_add",
                "name": "demo",
                "transport": "stdio",
                "command": "demo-mcp"
            }))
            .is_ok()
        );
        assert!(
            mcp.validate_input(&json!({
                "operation": "mcp_add",
                "name": "demo",
                "transport": "http",
                "command": "demo-mcp"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn config_projection_is_masked_and_typed_write_persists() {
        let (surfaces, _dir) = test_surfaces();
        let config = surfaces
            .execute(
                AdminRequest::Config(ConfigOperationArgs::ConfigGet { path: None }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(config.success);
        let written = surfaces
            .execute(
                AdminRequest::Config(ConfigOperationArgs::LogsLevel {
                    level: LogLevel::Warn,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(written.output["level"], json!("warn"));
        assert_eq!(written.output["saved"], json!(true));
    }

    #[tokio::test]
    async fn tool_toggle_and_mcp_add_use_their_typed_surface() {
        let (surfaces, _dir) = test_surfaces();
        let toggled = surfaces
            .execute(
                AdminRequest::Tools(ToolsOperationArgs::ToolDisable {
                    name: "shell".into(),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(toggled.output["name"], json!("shell"));
        assert_eq!(toggled.output["enabled"], json!(false));

        let added = surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpAdd {
                    name: "demo".into(),
                    transport: McpTransportType::Stdio,
                    command: Some("demo-mcp".into()),
                    url: None,
                    args: Vec::new(),
                    env: Vec::new(),
                    cwd: None,
                    enabled: false,
                    auto_connect: false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(added.output["name"], json!("demo"));
        assert_eq!(added.output["saved"], json!(true));
    }

    #[tokio::test]
    async fn llm_capability_mutations_rebuild_catalog_and_advance_mcp_clock() {
        let manager = Arc::new(ToolsManager::new());
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("demo");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Skill: demo\n\n## Metadata\n- description: demo\n\n## Instructions\nrun demo\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("scripts").join("main.py"), "print('{}')\n").unwrap();
        manager
            .share_services()
            .skills
            .set_config(Some(dir.path().to_path_buf()), None)
            .await
            .unwrap();

        let config_dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&config_dir.path().join("config.toml")).unwrap();
        let config_service = Arc::new(test_config_service(loader));
        let context = AdminContext {
            config_service: Some(config_service),
            config_apply_gate: None,
            session_store: None,
            memory_facts: None,
            router: None,
            log_path: None,
            file_logging_enabled: false,
            log_level: None,
            tool_control: Some(manager.tool_control_port()),
        };
        let surfaces = AdminSurfaces::new(
            context,
            manager.share_services().skills.clone(),
            Arc::new(manager.share_services().mcp.clone()),
            manager.share_services().mcp_configs.clone(),
            manager.registry().clone(),
            256 * 1024,
            512 * 1024,
        );

        let registry_before = manager.registry().version();
        surfaces
            .execute(
                AdminRequest::Skills(SkillsOperationArgs::SkillEnable {
                    name: "demo".into(),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(manager.registry().version() > registry_before);

        let mcp_before = manager.mcp_catalog_version();
        surfaces
            .execute(
                AdminRequest::Mcp(McpOperationArgs::McpAdd {
                    name: "disabled-server".into(),
                    transport: McpTransportType::Stdio,
                    command: Some("not-started".into()),
                    url: None,
                    args: Vec::new(),
                    env: Vec::new(),
                    cwd: None,
                    enabled: false,
                    auto_connect: false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(manager.mcp_catalog_version() > mcp_before);
    }

    #[test]
    fn typed_config_metadata_carries_operation_policy() {
        let (tool, _service, _dir) = config_tool();
        let get = json!({"operation": "config_get", "path": "log.level"});
        assert_eq!(tool.risk_level(&get), RiskLevel::Low);
        assert_eq!(tool.idempotency(&get), OperationIdempotency::Idempotent);
        assert_eq!(
            tool.concurrency(&get),
            ToolConcurrency::SharedResource("config".into())
        );
        assert_eq!(tool.timeout_secs_for(&get), 10);

        let set = json!({"operation": "logs_level", "level": "debug"});
        assert_eq!(tool.risk_level(&set), RiskLevel::Medium);
        assert_eq!(tool.idempotency(&set), OperationIdempotency::Idempotent);
        assert_eq!(
            tool.concurrency(&set),
            ToolConcurrency::Resource("config".into())
        );
    }

    #[tokio::test]
    async fn typed_config_operation_persists_and_reuses_idempotent_level() {
        let (tool, service, _dir) = config_tool();
        let view = tool
            .execute(
                json!({"operation": "config_get", "path": "log.level"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(view.output, json!("info"));

        let changed = tool
            .execute(
                json!({"operation": "logs_level", "level": "debug"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(changed.output["level"], json!("debug"));
        assert_eq!(changed.output["version"], json!(1));
        assert_eq!(
            service.snapshot().unwrap().config.log.level,
            LogLevel::Debug
        );

        let repeated = tool
            .execute(
                json!({"operation": "logs_level", "level": "debug"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(repeated.output["version"], json!(1));
        assert_eq!(service.snapshot().unwrap().version, 1);
    }

    #[tokio::test]
    async fn config_level_failure_after_persist_reports_unknown_side_effect() {
        struct FailingLogLevelPort;

        impl crate::LogLevelPort for FailingLogLevelPort {
            fn set_level(&self, _level: &LogLevel) -> anyhow::Result<()> {
                anyhow::bail!("host logger could not be reconfigured")
            }
        }

        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(test_config_service(loader));
        let tool = new_config_admin_tool(ConfigAdminContext {
            config_service: Some(service.clone()),
            config_apply_gate: None,
            log_level: Some(Arc::new(FailingLogLevelPort)),
        });

        let error = tool
            .execute(
                json!({"operation": "logs_level", "level": "debug"}),
                CancellationToken::new(),
            )
            .await
            .expect_err("host logger failure must be surfaced");
        let metadata = error
            .downcast_ref::<StructuredToolError>()
            .expect("side-effect uncertainty must remain structured")
            .metadata();
        assert_eq!(
            metadata,
            ToolErrorMetadata {
                class: crate::ToolErrorClass::SideEffectMayHaveHappened,
                outcome: ToolExecutionOutcome::Failed,
                retryability: crate::ToolRetryability::Unknown,
            }
        );
        assert_eq!(
            service.snapshot().unwrap().config.log.level,
            LogLevel::Debug
        );
        assert_eq!(service.snapshot().unwrap().version, 1);
    }

    #[tokio::test]
    async fn config_admin_write_waits_for_the_shared_apply_gate() {
        let (surfaces, service, _dir) = test_surfaces_with_service();
        let gate = surfaces
            .config
            .services
            .context
            .config_apply_gate
            .as_ref()
            .unwrap()
            .clone();
        let apply_guard = gate.lock_owned().await;
        let started = Arc::new(tokio::sync::Notify::new());
        let started_in_task = started.clone();
        let surfaces_in_task = surfaces.clone();

        let operation = tokio::spawn(async move {
            started_in_task.notify_one();
            surfaces_in_task
                .execute(
                    AdminRequest::Config(ConfigOperationArgs::LogsLevel {
                        level: LogLevel::Debug,
                    }),
                    CancellationToken::new(),
                )
                .await
        });

        started.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(service.snapshot().unwrap().version, 0);
        assert!(!operation.is_finished());

        drop(apply_guard);
        assert!(operation.await.unwrap().unwrap().success);
        let snapshot = service.snapshot().unwrap();
        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.config.log.level, LogLevel::Debug);
    }

    #[tokio::test]
    async fn typed_config_contract_rejects_unknown_missing_removed_and_cancelled() {
        let (tool, service, _dir) = config_tool();
        let unknown = json!({
            "operation": "config_get",
            "path": "log.level",
            "level": "debug"
        });
        assert!(tool.validate_input(&unknown).is_err());
        assert!(
            tool.execute(unknown, CancellationToken::new())
                .await
                .is_err()
        );

        let missing = json!({"operation": "logs_level"});
        assert!(tool.validate_input(&missing).is_err());
        let error = tool
            .execute(missing, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("missing field `level`"));

        let removed = json!({
            "operation": "config_set",
            "path": "session.max_concurrent",
            "value": 99
        });
        assert!(tool.validate_input(&removed).is_err());

        let missing_path = json!({
            "operation": "config_get",
            "path": "not.a.real.config.key"
        });
        let error = tool
            .execute(missing_path, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not.a.real.config.key"));

        let before = service.snapshot().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = tool
            .execute(json!({"operation": "logs_level", "level": "debug"}), cancel)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(service.snapshot().unwrap(), before);
    }

    #[tokio::test]
    async fn every_admin_operation_rejects_missing_unknown_and_cross_surface_input() {
        let (surfaces, _dir) = test_surfaces();
        let tools = surfaces.tools();
        let cases = admin_operation_cases();
        let mut seen = std::collections::HashSet::new();

        for case in cases {
            let tool = tool_for(&tools, case.surface);
            assert!(
                tool.validate_input(&case.input).is_ok(),
                "{}:{} rejected its valid input {}",
                case.surface,
                case.operation,
                case.input
            );
            assert!(seen.insert((case.surface, case.operation)));

            for field in case.required {
                assert_validation_rejection(
                    tool,
                    remove_field(&case.input, field),
                    &format!("{}:{} missing {field}", case.surface, case.operation),
                )
                .await;
            }

            let mut unknown = case.input.as_object().unwrap().clone();
            unknown.insert("unexpected_field".into(), json!(true));
            assert_validation_rejection(
                tool,
                Value::Object(unknown),
                &format!("{}:{} unknown field", case.surface, case.operation),
            )
            .await;
        }

        // The selector is a closed union: an operation from another surface
        // must not be guessed or dispatched by its spelling.
        for (surface, input) in [
            (
                "haven_diagnostics",
                json!({"operation": "logs_level", "level": "debug"}),
            ),
            (
                "haven_config",
                json!({"operation": "skill_create", "name": "x"}),
            ),
            (
                "haven_skills",
                json!({"operation": "tool_disable", "name": "shell"}),
            ),
            (
                "haven_tools",
                json!({"operation": "mcp_remove", "name": "demo"}),
            ),
            ("haven_mcp", json!({"operation": "config_get"})),
        ] {
            assert_validation_rejection(tool_for(&tools, surface), input, surface).await;
        }

        assert_eq!(
            seen.len(),
            21,
            "operation matrix must cover every admin operation"
        );
    }

    #[test]
    fn every_admin_operation_declares_duplicate_and_timeout_policy() {
        let (surfaces, _dir) = test_surfaces();
        let tools = surfaces.tools();

        for case in admin_operation_cases() {
            let tool = tool_for(&tools, case.surface);
            assert_eq!(
                tool.idempotency(&case.input),
                case.idempotency,
                "duplicate-call policy drift for {}:{}",
                case.surface,
                case.operation
            );
            assert_eq!(
                tool.timeout_secs_for(&case.input),
                10,
                "timeout policy drift for {}:{}",
                case.surface,
                case.operation
            );
            assert_eq!(
                tool.timeout_outcome(),
                ToolExecutionOutcome::TimedOutAndTerminated,
                "admin operations must use the declared terminating timeout contract"
            );
        }
    }

    #[tokio::test]
    async fn every_admin_operation_is_cancellation_safe_before_side_effects() {
        let (surfaces, _dir) = test_surfaces();
        let tools = surfaces.tools();

        for case in admin_operation_cases() {
            let cancel = CancellationToken::new();
            cancel.cancel();
            let error = tool_for(&tools, case.surface)
                .execute(case.input, cancel)
                .await
                .expect_err("cancelled admin operation must not execute");
            let structured = error
                .downcast_ref::<StructuredToolError>()
                .unwrap_or_else(|| {
                    panic!("{} lost structured cancellation metadata", case.operation)
                });
            assert_eq!(
                structured.metadata(),
                ToolErrorMetadata {
                    class: crate::ToolErrorClass::UnknownOutcome,
                    outcome: ToolExecutionOutcome::Cancelled,
                    retryability: crate::ToolRetryability::Unknown,
                },
                "cancellation contract drift for {}:{}",
                case.surface,
                case.operation
            );
        }
    }

    #[tokio::test]
    async fn duplicate_admin_calls_are_bounded_and_unknown_side_effects_are_not_replayed() {
        let (surfaces, service, _dir) = test_surfaces_with_service();
        let tools = surfaces.tools();

        let level = json!({"operation": "logs_level", "level": "debug"});
        let first = tool_for(&tools, "haven_config")
            .execute(level.clone(), CancellationToken::new())
            .await
            .unwrap();
        let second = tool_for(&tools, "haven_config")
            .execute(level, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(first.output["version"], json!(1));
        assert_eq!(second.output["version"], json!(1));
        assert_eq!(service.snapshot().unwrap().version, 1);

        let add = json!({
            "operation": "mcp_add",
            "name": "duplicate-server",
            "transport": "stdio",
            "command": "not-started",
            "enabled": false,
            "auto_connect": false,
        });
        tool_for(&tools, "haven_mcp")
            .execute(add.clone(), CancellationToken::new())
            .await
            .unwrap();
        tool_for(&tools, "haven_mcp")
            .execute(add, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            service
                .snapshot()
                .unwrap()
                .config
                .mcp_servers
                .iter()
                .filter(|server| server.name == "duplicate-server")
                .count(),
            1,
            "duplicate mcp_add must not append a second server"
        );

        // Unknown-idempotency operations are never eligible for automatic
        // replay, even when their first call has already changed state.
        for case in admin_operation_cases()
            .into_iter()
            .filter(|case| case.idempotency == OperationIdempotency::Unknown)
        {
            assert_eq!(
                tool_for(&tools, case.surface).idempotency(&case.input),
                OperationIdempotency::Unknown,
                "{}:{} must require verification before replay",
                case.surface,
                case.operation
            );
        }
    }

    #[test]
    fn admin_error_metadata_preserves_cancelled_validation_and_unknown_side_effect_states() {
        let cancelled = AdminOperationError::cancelled();
        assert_eq!(
            operation_error_metadata(&cancelled),
            ToolErrorMetadata {
                class: crate::ToolErrorClass::UnknownOutcome,
                outcome: ToolExecutionOutcome::Cancelled,
                retryability: crate::ToolRetryability::Unknown,
            }
        );

        let validation = AdminOperationError::validation("not allowed");
        assert_eq!(
            operation_error_metadata(&validation),
            ToolErrorMetadata::validation()
        );

        let unknown = AdminOperationError::side_effect_may_have_happened("write may have landed");
        assert_eq!(
            operation_error_metadata(&unknown),
            ToolErrorMetadata {
                class: crate::ToolErrorClass::SideEffectMayHaveHappened,
                outcome: ToolExecutionOutcome::Failed,
                retryability: crate::ToolRetryability::Unknown,
            }
        );
    }

    fn test_surfaces_with_service() -> (AdminSurfaces, Arc<ConfigService>, TempDir) {
        let dir = TempDir::new().expect("temporary config directory");
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(test_config_service(loader));
        let context = AdminContext {
            config_service: Some(service.clone()),
            config_apply_gate: Some(Arc::new(tokio::sync::Mutex::new(()))),
            session_store: None,
            memory_facts: None,
            router: None,
            log_path: Some(dir.path().join("logs").join("haven.log")),
            file_logging_enabled: true,
            log_level: None,
            tool_control: None,
        };
        let surfaces = AdminSurfaces::new(
            context,
            SkillsEngine::new(),
            Arc::new(McpManager::new()),
            Arc::new(RwLock::new(HashMap::new())),
            ToolRegistry::new(),
            256 * 1024,
            512 * 1024,
        );
        (surfaces, service, dir)
    }
}
