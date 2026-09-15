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
use haven_mcp::McpManager;
use haven_memory::Database;
use haven_skills::SkillsEngine;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use admin_services::AdminServices;
pub(crate) use admin_services::sanitize_diagnostic;

/// App-provided dependencies shared by all five admin surfaces.
#[derive(Clone)]
pub struct AdminContext {
    pub config_service: Option<Arc<ConfigService>>,
    pub db: Option<Arc<Database>>,
    pub router: Option<Arc<LlmRouter>>,
    pub log_path: Option<PathBuf>,
    pub log_level: Option<Arc<dyn LogLevelPort>>,
    pub tool_control: Option<Arc<dyn ToolControlPort>>,
}

/// Narrow context retained for callers that construct only the config
/// operation. It contains no generic operation dispatcher state.
#[derive(Clone)]
pub struct ConfigAdminContext {
    pub config_service: Option<Arc<ConfigService>>,
    pub log_level: Option<Arc<dyn LogLevelPort>>,
}

impl From<ConfigAdminContext> for AdminContext {
    fn from(context: ConfigAdminContext) -> Self {
        Self {
            config_service: context.config_service,
            db: None,
            router: None,
            log_path: None,
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
    "actions",
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
}

impl AdminRequest {
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Diagnostics(_) => "haven_diagnostics",
            Self::Config(_) => "haven_config",
            Self::Skills(_) => "haven_skills",
            Self::Tools(_) => "haven_tools",
            Self::Mcp(_) => "haven_mcp",
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
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdminOperationOutput(Value);

impl AdminOperationOutput {
    fn new(value: Value) -> Self {
        Self(value)
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
}

impl Display for AdminOperationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn service_error(error: anyhow::Error) -> AdminOperationError {
    AdminOperationError::other(error.to_string())
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
        let value = match args {
            DiagnosticsOperationArgs::Status => self.services.diagnostics_status().await,
            DiagnosticsOperationArgs::LogsTail { limit } => self.services.logs_tail(limit).await,
            DiagnosticsOperationArgs::Sessions { limit } => self.services.sessions(limit).await,
            DiagnosticsOperationArgs::Errors { limit } => self.services.errors(limit).await,
        }
        .map_err(service_error)?;
        Ok(AdminOperationOutput::new(value))
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
                serde_json::json!({"name": {"type": "string", "minLength": 1}, "description": {"type": "string", "minLength": 1}, "instructions": {"type": "string", "minLength": 1}, "language": {"type": "string", "enum": ["python"]}, "version": {"type": "string"}, "script": {"type": "string"}}),
                &["operation", "name", "description", "instructions"],
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
        let value = match args {
            SkillsOperationArgs::SkillsList => self.services.skills_list().await,
            SkillsOperationArgs::SkillEnable { name } => self.services.skill_set(&name, true).await,
            SkillsOperationArgs::SkillDisable { name } => {
                self.services.skill_set(&name, false).await
            }
            SkillsOperationArgs::SkillCreate {
                name,
                description,
                instructions,
                language,
                version,
                script,
            } => {
                self.services
                    .skill_create(
                        &name,
                        &description,
                        &instructions,
                        language.as_deref(),
                        version.as_deref(),
                        script.as_deref(),
                    )
                    .await
            }
        }
        .map_err(service_error)?;
        Ok(AdminOperationOutput::new(value))
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
        Ok(AdminOperationOutput::new(
            self.services
                .tool_set(&name, enabled)
                .await
                .map_err(service_error)?,
        ))
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
        let value = match args {
            McpOperationArgs::McpList => self.services.mcp_status().await,
            McpOperationArgs::McpConnect { name } => self.services.mcp_connect(&name).await,
            McpOperationArgs::McpDisconnect { name } => self.services.mcp_disconnect(&name).await,
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
            } => {
                self.services
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
            }
            McpOperationArgs::McpUpdate {
                name,
                transport,
                command,
                url,
                args,
                env,
                cwd,
                enabled,
            } => {
                self.services
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
            }
            McpOperationArgs::McpToggle { name, enabled } => {
                self.services.mcp_toggle(&name, enabled).await
            }
            McpOperationArgs::McpRemove { name } => self.services.mcp_remove(&name).await,
            McpOperationArgs::McpReload => self.services.mcp_reload().await,
        }
        .map_err(service_error)?;
        Ok(AdminOperationOutput::new(value))
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
    Failed,
}

impl Display for ConfigOperationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("configuration operation cancelled"),
            Self::Unavailable => formatter.write_str("configuration administration is unavailable"),
            Self::PathNotFound { path } => write!(formatter, "config key '{}' not found", path),
            Self::Failed => formatter.write_str("configuration operation failed"),
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
                let value = self
                    .services
                    .logs_level(level.clone())
                    .await
                    .map_err(|_| ConfigOperationError::Failed)?;
                Ok(ConfigOperationOutput::LogsLevel(LogLevelOutput {
                    level,
                    saved: true,
                    version: value["version"].as_u64().unwrap_or_default(),
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
        }
    }

    pub async fn execute(
        &self,
        request: AdminRequest,
        cancel: CancellationToken,
    ) -> Result<ToolResult, AdminOperationError> {
        match request {
            AdminRequest::Diagnostics(args) => {
                let output = self
                    .diagnostics
                    .execute_typed(args, cancel)
                    .await
                    .map_err(|error| AdminOperationError::other(error.to_string()))?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::Config(args) => {
                let output = self
                    .config
                    .execute_typed(args, cancel)
                    .await
                    .map_err(|error| AdminOperationError::other(error.to_string()))?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::Skills(args) => {
                let output = self
                    .skills
                    .execute_typed(args, cancel)
                    .await
                    .map_err(|error| AdminOperationError::other(error.to_string()))?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::Tools(args) => {
                let output = self
                    .tools
                    .execute_typed(args, cancel)
                    .await
                    .map_err(|error| AdminOperationError::other(error.to_string()))?;
                Ok(ToolResult::ok(serde_json::to_value(output).map_err(
                    |error| AdminOperationError::other(error.to_string()),
                )?))
            }
            AdminRequest::Mcp(args) => {
                let output = self
                    .mcp
                    .execute_typed(args, cancel)
                    .await
                    .map_err(|error| AdminOperationError::other(error.to_string()))?;
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
    use crate::Tool;
    use haven_common::config::{ConfigLoader, ConfigService};
    use serde_json::json;
    use tempfile::TempDir;

    fn test_surfaces() -> (AdminSurfaces, TempDir) {
        let dir = TempDir::new().expect("temporary config directory");
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let context = AdminContext {
            config_service: Some(Arc::new(ConfigService::new(loader))),
            db: None,
            router: None,
            log_path: Some(dir.path().join("logs").join("haven.log")),
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
        let service = Arc::new(ConfigService::new(loader));
        let tool = new_config_admin_tool(ConfigAdminContext {
            config_service: Some(service.clone()),
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
}
