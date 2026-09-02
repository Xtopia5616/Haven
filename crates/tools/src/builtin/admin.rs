//! Model-facing administration capabilities.
//!
//! The implementation of the admin services is still shared with the native
//! app commands through [`SelfTool`]. This module is the important boundary:
//! the model never receives that broad dispatcher. Each registered tool gets
//! one capability and an allowlisted operation schema, so the safety gateway
//! sees a stable tool/capability key before any side effect can run.

use super::admin_support::{mask_sensitive_config, value_at};
use super::self_tool::{SelfOperation, SelfParams, SelfTool, sanitize_diagnostic};
use crate::{
    OperationIdempotency, Tool, ToolCancellationPolicy, ToolConcurrency, ToolDef,
    ToolOperationMetadata, ToolOperationScope, ToolResult, TypedToolAdapter, TypedToolOperation,
};
use async_trait::async_trait;
use haven_common::config::{ConfigPatch, ConfigService, LogLevel};
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The narrow administration capabilities exposed to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminCapability {
    Diagnostics,
    Config,
    Skills,
    Tools,
    Mcp,
    SessionDiagnostics,
}

impl AdminCapability {
    pub const ALL: [Self; 6] = [
        Self::Diagnostics,
        Self::Config,
        Self::Skills,
        Self::Tools,
        Self::Mcp,
        Self::SessionDiagnostics,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Diagnostics => "haven_diagnostics",
            Self::Config => "haven_config",
            Self::Skills => "haven_skills",
            Self::Tools => "haven_tools",
            Self::Mcp => "haven_mcp",
            Self::SessionDiagnostics => "haven_session_diagnostics",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Diagnostics => {
                "Inspect Haven health and bounded log output without changing state."
            }
            Self::Config => "Read masked configuration and change the typed runtime log level.",
            Self::Skills => "List, enable, disable, or create Haven skills.",
            Self::Tools => "Enable or disable a builtin Haven tool.",
            Self::Mcp => "Inspect and manage configured MCP servers and connections.",
            Self::SessionDiagnostics => "Inspect bounded session history and recent errors.",
        }
    }

    fn operations(self) -> &'static [SelfOperation] {
        match self {
            Self::Diagnostics => &[SelfOperation::Status, SelfOperation::LogsTail],
            Self::Config => &[SelfOperation::ConfigGet, SelfOperation::LogsLevel],
            Self::Skills => &[
                SelfOperation::SkillsList,
                SelfOperation::SkillEnable,
                SelfOperation::SkillDisable,
                SelfOperation::SkillCreate,
            ],
            Self::Tools => &[SelfOperation::ToolEnable, SelfOperation::ToolDisable],
            Self::Mcp => &[
                SelfOperation::McpList,
                SelfOperation::McpConnect,
                SelfOperation::McpDisconnect,
                SelfOperation::McpAdd,
                SelfOperation::McpUpdate,
                SelfOperation::McpToggle,
                SelfOperation::McpRemove,
                SelfOperation::McpReload,
            ],
            Self::SessionDiagnostics => &[SelfOperation::Sessions, SelfOperation::Errors],
        }
    }

    fn accepts(self, operation: SelfOperation) -> bool {
        self.operations().contains(&operation)
    }

    fn metadata(self, operation: SelfOperation) -> Option<AdminOperationMetadata> {
        if !self.accepts(operation) {
            return None;
        }
        Some(AdminOperationMetadata {
            capability: self,
            risk_level: operation.risk_level(),
            session_scoped: false,
            retryable: operation.is_read_only(),
        })
    }

    fn operation_names(self) -> Vec<&'static str> {
        self.operations()
            .iter()
            .map(|operation| match operation {
                SelfOperation::Status => "status",
                SelfOperation::ConfigGet => "config_get",
                SelfOperation::SkillsList => "skills_list",
                SelfOperation::SkillEnable => "skill_enable",
                SelfOperation::SkillDisable => "skill_disable",
                SelfOperation::SkillCreate => "skill_create",
                SelfOperation::ToolEnable => "tool_enable",
                SelfOperation::ToolDisable => "tool_disable",
                SelfOperation::McpList => "mcp_list",
                SelfOperation::McpConnect => "mcp_connect",
                SelfOperation::McpDisconnect => "mcp_disconnect",
                SelfOperation::McpAdd => "mcp_add",
                SelfOperation::McpUpdate => "mcp_update",
                SelfOperation::McpToggle => "mcp_toggle",
                SelfOperation::McpRemove => "mcp_remove",
                SelfOperation::McpReload => "mcp_reload",
                SelfOperation::LogsTail => "logs_tail",
                SelfOperation::LogsLevel => "logs_level",
                SelfOperation::Sessions => "sessions",
                SelfOperation::Errors => "errors",
            })
            .collect()
    }

    fn schema(self) -> Value {
        let mut properties = serde_json::Map::new();
        properties.insert(
            "operation".into(),
            serde_json::json!({
                "type": "string",
                "enum": self.operation_names(),
                "description": "The allowlisted administration operation"
            }),
        );

        match self {
            Self::Diagnostics | Self::SessionDiagnostics => {
                properties.insert(
                    "limit".into(),
                    serde_json::json!({
                        "type": "integer",
                        "description": "Bounded row/line limit"
                    }),
                );
            }
            Self::Config => {
                properties.insert(
                    "path".into(),
                    serde_json::json!({
                        "type": "string",
                        "description": "Optional read-only config path; values are masked"
                    }),
                );
                properties.insert(
                    "level".into(),
                    serde_json::json!({
                        "type": "string",
                        "enum": ["trace", "debug", "info", "warn", "error"],
                        "description": "Log level for logs_level"
                    }),
                );
            }
            Self::Skills => {
                add_skill_properties(&mut properties);
            }
            Self::Tools => {
                properties.insert(
                    "name".into(),
                    serde_json::json!({
                        "type": "string",
                        "description": "Builtin tool name"
                    }),
                );
            }
            Self::Mcp => {
                add_mcp_properties(&mut properties);
            }
        }

        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
            "required": ["operation"]
        })
    }
}

/// Stable policy metadata for one admin operation. The current `Tool` trait
/// exposes risk and idempotency separately; keeping the rest of the policy
/// together here prevents a future operation from silently omitting its
/// capability or scope declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminOperationMetadata {
    pub capability: AdminCapability,
    pub risk_level: RiskLevel,
    pub session_scoped: bool,
    pub retryable: bool,
}

/// Native dependencies needed by the typed configuration operation. Keeping
/// this context smaller than `SelfToolContext` is intentional: the config
/// contract must not acquire MCP, skills, database, or registry dependencies.
#[derive(Clone)]
pub struct ConfigAdminContext {
    pub config_service: Option<Arc<ConfigService>>,
    pub set_log_level: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

/// Typed arguments for the `haven_config` grouped tool. The serde tag is the
/// sole provider-boundary operation selector; every variant carries only the
/// fields that its operation can consume.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigViewOutput {
    /// A deliberately bounded/masked projection. The dynamic value is kept
    /// at this read-only inspection boundary because config sections evolve
    /// independently from the operation contract.
    pub value: Value,
}

#[derive(Debug, Clone, serde::Serialize)]
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

impl serde::Serialize for ConfigOperationOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            // Preserve the existing model-facing config_get shape while the
            // internal result remains a typed variant.
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

impl std::fmt::Display for ConfigOperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("configuration operation cancelled"),
            Self::Unavailable => formatter.write_str("configuration administration is unavailable"),
            Self::PathNotFound { path } => write!(formatter, "config key '{path}' not found"),
            Self::Failed => formatter.write_str("configuration operation failed"),
        }
    }
}

/// Typed implementation of the `haven_config` capability. It owns the
/// operation-specific metadata and execution, while `TypedToolAdapter` is
/// responsible only for converting provider JSON at the edge.
pub struct ConfigAdminOperation {
    context: ConfigAdminContext,
}

impl ConfigAdminOperation {
    pub fn new(context: ConfigAdminContext) -> Self {
        Self { context }
    }

    fn metadata_for(args: &ConfigOperationArgs) -> ToolOperationMetadata {
        match args {
            ConfigOperationArgs::ConfigGet { .. } => ToolOperationMetadata {
                capability: "haven_config",
                operation: "config_get",
                scope: ToolOperationScope::Global,
                risk_level: RiskLevel::Low,
                idempotency: OperationIdempotency::Idempotent,
                cancellation: ToolCancellationPolicy::Terminating,
                timeout_secs: 10,
                concurrency: ToolConcurrency::SharedResource("config".into()),
            },
            ConfigOperationArgs::LogsLevel { .. } => ToolOperationMetadata {
                capability: "haven_config",
                operation: "logs_level",
                scope: ToolOperationScope::Global,
                risk_level: RiskLevel::Medium,
                idempotency: OperationIdempotency::Idempotent,
                cancellation: ToolCancellationPolicy::Terminating,
                timeout_secs: 10,
                concurrency: ToolConcurrency::Resource("config".into()),
            },
        }
    }
}

#[async_trait]
impl TypedToolOperation for ConfigAdminOperation {
    type Args = ConfigOperationArgs;
    type Output = ConfigOperationOutput;
    type Error = ConfigOperationError;

    fn metadata(&self, args: &Self::Args) -> ToolOperationMetadata {
        Self::metadata_for(args)
    }

    fn default_metadata(&self) -> ToolOperationMetadata {
        // The grouped ToolDef advertises the most conservative operation.
        Self::metadata_for(&ConfigOperationArgs::LogsLevel {
            level: LogLevel::Info,
        })
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "config_get" },
                        "path": { "type": "string", "description": "Optional masked config path" }
                    },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "logs_level" },
                        "level": { "type": "string", "enum": ["trace", "debug", "info", "warn", "error"] }
                    },
                    "required": ["operation", "level"]
                }
            ]
        })
    }

    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        if cancel.is_cancelled() {
            return Err(ConfigOperationError::Cancelled);
        }
        let service = self
            .context
            .config_service
            .as_ref()
            .ok_or(ConfigOperationError::Unavailable)?;

        match args {
            ConfigOperationArgs::ConfigGet { path } => {
                let snapshot = service
                    .snapshot()
                    .map_err(|_| ConfigOperationError::Failed)?;
                let mut root = if path.as_deref().is_none_or(str::is_empty) {
                    serde_json::to_value(
                        service
                            .settings()
                            .map_err(|_| ConfigOperationError::Failed)?,
                    )
                    .map_err(|_| ConfigOperationError::Failed)?
                } else {
                    serde_json::to_value(&snapshot.config)
                        .map_err(|_| ConfigOperationError::Failed)?
                };
                mask_sensitive_config(&mut root);
                let value = match path.as_deref().filter(|path| !path.is_empty()) {
                    Some(path) => value_at(&root, path)
                        .ok_or_else(|| ConfigOperationError::PathNotFound {
                            path: path.to_string(),
                        })?
                        .clone(),
                    None => root,
                };
                Ok(ConfigOperationOutput::Config(ConfigViewOutput { value }))
            }
            ConfigOperationArgs::LogsLevel { level } => {
                let update = service
                    .apply_patch(ConfigPatch::LogLevel(level.clone()))
                    .map_err(|_| ConfigOperationError::Failed)?;
                if let Some(set_log_level) = &self.context.set_log_level {
                    set_log_level(level.as_str().to_string());
                }
                Ok(ConfigOperationOutput::LogsLevel(LogLevelOutput {
                    level,
                    saved: true,
                    version: update.snapshot.version,
                }))
            }
        }
    }
}

/// The provider-facing adapter for the typed config operation.
pub type ConfigAdminTool = TypedToolAdapter<ConfigAdminOperation>;

pub fn new_config_admin_tool(context: ConfigAdminContext) -> ConfigAdminTool {
    TypedToolAdapter::new(
        "haven_config",
        "Read masked configuration or change the typed runtime log level.",
        ConfigAdminOperation::new(context),
    )
}

impl SelfOperation {
    fn is_read_only(self) -> bool {
        matches!(
            self,
            Self::Status
                | Self::ConfigGet
                | Self::SkillsList
                | Self::McpList
                | Self::LogsTail
                | Self::Sessions
                | Self::Errors
        )
    }

    fn risk_level(self) -> RiskLevel {
        match self {
            Self::Status
            | Self::ConfigGet
            | Self::SkillsList
            | Self::McpList
            | Self::LogsTail
            | Self::Sessions
            | Self::Errors => RiskLevel::Low,
            Self::McpConnect | Self::McpDisconnect | Self::McpReload | Self::LogsLevel => {
                RiskLevel::Medium
            }
            Self::SkillCreate
            | Self::McpAdd
            | Self::McpUpdate
            | Self::McpToggle
            | Self::McpRemove => RiskLevel::High,
            Self::SkillEnable | Self::SkillDisable | Self::ToolEnable | Self::ToolDisable => {
                RiskLevel::Medium
            }
        }
    }
}

fn add_skill_properties(properties: &mut serde_json::Map<String, Value>) {
    properties.insert(
        "name".into(),
        serde_json::json!({ "type": "string", "description": "Skill name" }),
    );
    properties.insert(
        "description".into(),
        serde_json::json!({ "type": "string", "description": "Description for skill_create" }),
    );
    properties.insert(
        "instructions".into(),
        serde_json::json!({ "type": "string", "description": "Instructions for skill_create" }),
    );
    properties.insert(
        "language".into(),
        serde_json::json!({ "type": "string", "enum": ["python"] }),
    );
    properties.insert(
        "version".into(),
        serde_json::json!({ "type": "string", "description": "Optional skill version" }),
    );
    properties.insert(
        "script".into(),
        serde_json::json!({ "type": "string", "description": "Optional scripts/main.py body" }),
    );
}

fn add_mcp_properties(properties: &mut serde_json::Map<String, Value>) {
    properties.insert(
        "name".into(),
        serde_json::json!({ "type": "string", "description": "MCP server name" }),
    );
    properties.insert(
        "command".into(),
        serde_json::json!({ "type": "string", "description": "stdio server command" }),
    );
    properties.insert(
        "transport".into(),
        serde_json::json!({ "type": "string", "enum": ["stdio", "http"] }),
    );
    properties.insert(
        "url".into(),
        serde_json::json!({ "type": "string", "description": "HTTP server endpoint" }),
    );
    properties.insert(
        "args".into(),
        serde_json::json!({ "type": "array", "items": { "type": "string" } }),
    );
    properties.insert(
        "env".into(),
        serde_json::json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "KEY=VALUE entries; values are never returned"
        }),
    );
    properties.insert(
        "cwd".into(),
        serde_json::json!({ "type": "string", "description": "stdio working directory" }),
    );
    properties.insert("enabled".into(), serde_json::json!({ "type": "boolean" }));
    properties.insert(
        "auto_connect".into(),
        serde_json::json!({ "type": "boolean", "description": "Connect after mcp_add" }),
    );
}

/// A model-facing adapter for one admin capability.
pub struct AdminCapabilityTool {
    surface: Arc<SelfTool>,
    capability: AdminCapability,
}

impl AdminCapabilityTool {
    pub fn new(surface: Arc<SelfTool>, capability: AdminCapability) -> Self {
        Self {
            surface,
            capability,
        }
    }
}

#[async_trait]
impl Tool for AdminCapabilityTool {
    fn name(&self) -> String {
        self.capability.name().into()
    }

    fn description(&self) -> String {
        self.capability.description().into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        self.operation(input)
            .and_then(|operation| self.capability.metadata(operation))
            .map(|metadata| metadata.risk_level)
            .unwrap_or(RiskLevel::High)
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match self
            .operation(input)
            .and_then(|operation| self.capability.metadata(operation))
        {
            Some(metadata) if metadata.retryable => OperationIdempotency::Idempotent,
            _ => OperationIdempotency::Unknown,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        self.surface.concurrency(input)
    }

    fn input_schema(&self) -> Value {
        self.capability.schema()
    }

    fn tool_def(&self) -> ToolDef {
        ToolDef::new(
            self.name(),
            self.description(),
            self.input_schema(),
            self.capability
                .operations()
                .iter()
                .copied()
                .map(SelfOperation::risk_level)
                .fold(RiskLevel::Safe, |current_max, risk| {
                    if risk > current_max {
                        risk
                    } else {
                        current_max
                    }
                }),
        )
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<SelfParams>(&self.name(), input)?;
        if !self.capability.accepts(params.operation) {
            anyhow::bail!(
                "operation {:?} is not available through {}",
                params.operation,
                self.name()
            );
        }
        self.surface
            .run(params, cancel)
            .await
            .map_err(|error| anyhow::anyhow!(sanitize_diagnostic(&error.to_string())))
    }
}

impl AdminCapabilityTool {
    fn operation(&self, input: &Value) -> Option<SelfOperation> {
        serde_json::from_value(input.get("operation")?.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{ConfigLoader, ConfigService};
    use haven_mcp::McpManager;
    use haven_skills::SkillsEngine;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    fn test_surface() -> (Arc<SelfTool>, TempDir) {
        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let context = super::super::self_tool::SelfToolContext {
            config_service: Some(Arc::new(ConfigService::new(loader))),
            db: None,
            router: None,
            log_path: None,
            set_log_level: None,
            tools_weak: None,
        };
        (
            Arc::new(SelfTool::new(
                context,
                SkillsEngine::new(),
                Arc::new(McpManager::new()),
                Arc::new(RwLock::new(HashMap::new())),
                crate::ToolRegistry::new(),
                256 * 1024,
                512 * 1024,
            )),
            dir,
        )
    }

    #[test]
    fn capabilities_have_disjoint_operation_surfaces() {
        for capability in AdminCapability::ALL {
            let schema = capability.schema();
            let operations = schema["properties"]["operation"]["enum"]
                .as_array()
                .expect("operation enum");
            assert!(!operations.is_empty());
            assert!(schema["additionalProperties"] == false);
        }

        assert!(!AdminCapability::Config.accepts(SelfOperation::McpRemove));
        assert!(!AdminCapability::Diagnostics.accepts(SelfOperation::SkillCreate));
        assert_eq!(
            AdminCapability::Mcp
                .metadata(SelfOperation::McpRemove)
                .unwrap()
                .risk_level,
            RiskLevel::High
        );
        assert!(
            AdminCapability::Diagnostics
                .metadata(SelfOperation::LogsTail)
                .unwrap()
                .retryable
        );
    }

    #[tokio::test]
    async fn capability_adapter_rejects_cross_domain_operation() {
        let (surface, _dir) = test_surface();
        let diagnostics = AdminCapabilityTool::new(surface, AdminCapability::Diagnostics);
        let error = diagnostics
            .execute(
                serde_json::json!({ "operation": "mcp_remove", "name": "server" }),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not available through haven_diagnostics")
        );
    }

    fn config_tool() -> (ConfigAdminTool, Arc<ConfigService>, TempDir) {
        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(ConfigService::new(loader));
        let tool = new_config_admin_tool(ConfigAdminContext {
            config_service: Some(service.clone()),
            set_log_level: None,
        });
        (tool, service, dir)
    }

    #[test]
    fn typed_config_metadata_carries_operation_policy() {
        let (tool, _service, _dir) = config_tool();
        let get = serde_json::json!({
            "operation": "config_get",
            "path": "log.level"
        });
        assert_eq!(tool.risk_level(&get), RiskLevel::Low);
        assert_eq!(tool.idempotency(&get), OperationIdempotency::Idempotent);
        assert_eq!(
            tool.concurrency(&get),
            ToolConcurrency::SharedResource("config".into())
        );
        assert_eq!(tool.timeout_secs_for(&get), 10);

        let set = serde_json::json!({
            "operation": "logs_level",
            "level": "debug"
        });
        assert_eq!(tool.risk_level(&set), RiskLevel::Medium);
        assert_eq!(tool.idempotency(&set), OperationIdempotency::Idempotent);
        assert_eq!(
            tool.concurrency(&set),
            ToolConcurrency::Resource("config".into())
        );
    }

    #[tokio::test]
    async fn typed_config_operation_returns_typed_outputs_and_persists_level() {
        let (tool, service, _dir) = config_tool();
        let view = tool
            .execute(
                serde_json::json!({
                    "operation": "config_get",
                    "path": "log.level"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(view.output, serde_json::json!("info"));

        let changed = tool
            .execute(
                serde_json::json!({
                    "operation": "logs_level",
                    "level": "debug"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(changed.output["level"], serde_json::json!("debug"));
        assert_eq!(changed.output["saved"], serde_json::json!(true));
        assert_eq!(changed.output["version"], serde_json::json!(1));
        assert_eq!(
            service.snapshot().unwrap().config.log.level,
            LogLevel::Debug
        );

        let repeated = tool
            .execute(
                serde_json::json!({
                    "operation": "logs_level",
                    "level": "debug"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(repeated.output["version"], serde_json::json!(1));
        assert_eq!(service.snapshot().unwrap().version, 1);
    }

    #[tokio::test]
    async fn typed_config_contract_rejects_unknown_or_missing_fields() {
        let (tool, _service, _dir) = config_tool();
        let unknown = serde_json::json!({
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

        let missing = serde_json::json!({ "operation": "logs_level" });
        assert!(tool.validate_input(&missing).is_err());
        let error = tool
            .execute(missing, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("missing field `level`"));

        let removed = serde_json::json!({
            "operation": "config_set",
            "path": "session.max_concurrent",
            "value": 99
        });
        assert!(tool.validate_input(&removed).is_err());
        assert!(
            tool.execute(removed, CancellationToken::new())
                .await
                .is_err()
        );

        let missing_path = serde_json::json!({
            "operation": "config_get",
            "path": "not.a.real.config.key"
        });
        let error = tool
            .execute(missing_path, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not.a.real.config.key"));
    }

    #[tokio::test]
    async fn typed_config_operation_is_cancelled_before_side_effect() {
        let (tool, service, _dir) = config_tool();
        let before = service.snapshot().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = tool
            .execute(
                serde_json::json!({
                    "operation": "logs_level",
                    "level": "debug"
                }),
                cancel,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(service.snapshot().unwrap(), before);
    }
}
