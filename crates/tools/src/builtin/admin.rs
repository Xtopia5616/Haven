//! Model-facing administration capabilities.
//!
//! The implementation of the admin services is still shared with the native
//! app commands through [`SelfTool`]. This module is the important boundary:
//! the model never receives that broad dispatcher. Each registered tool gets
//! one capability and an allowlisted operation schema, so the safety gateway
//! sees a stable tool/capability key before any side effect can run.

use super::self_tool::{SelfOperation, SelfParams, SelfTool, sanitize_diagnostic};
use crate::{OperationIdempotency, Tool, ToolConcurrency, ToolDef, ToolResult};
use async_trait::async_trait;
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
}
