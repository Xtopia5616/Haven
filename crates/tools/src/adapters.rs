use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::mcp::{McpClient, McpToolInfo};
use crate::skills::Skill;
use crate::skills::runner::SkillRunner;
use crate::{Tool, ToolResult};

// ---------------------------------------------------------------------------
// McpToolAdapter — wraps an MCP client tool as a dyn Tool
// ---------------------------------------------------------------------------

pub struct McpToolAdapter {
    client: Arc<McpClient>,
    info: McpToolInfo,
    server_name: String,
}

impl McpToolAdapter {
    pub fn new(client: Arc<McpClient>, server_name: &str, info: McpToolInfo) -> Self {
        Self {
            client,
            info,
            server_name: server_name.into(),
        }
    }

    fn qualified_name(&self) -> String {
        format!("mcp::{}::{}", self.server_name, self.info.name)
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> String {
        self.qualified_name()
    }

    fn description(&self) -> String {
        self.info.description.clone()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn input_schema(&self) -> Value {
        self.info.input_schema.clone()
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        self.client.call_tool(&self.info.name, input, cancel).await
    }
}

// ---------------------------------------------------------------------------
// SkillToolAdapter — wraps a Skill as a dyn Tool
// ---------------------------------------------------------------------------

pub struct SkillToolAdapter {
    skill: Arc<Skill>,
    runner: SkillRunner,
}

impl SkillToolAdapter {
    pub fn new(skill: Arc<Skill>, runner: SkillRunner) -> Self {
        Self { skill, runner }
    }

    fn qualified_name(&self) -> String {
        format!("skill::{}", self.skill.name())
    }
}

#[async_trait]
impl Tool for SkillToolAdapter {
    fn name(&self) -> String {
        self.qualified_name()
    }

    fn description(&self) -> String {
        self.skill.description().to_string()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "params": {
                    "type": "object",
                    "description": "Skill-specific parameters passed as JSON object"
                }
            },
            "required": ["params"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = input.get("params").cloned().unwrap_or(input);
        self.runner.execute(&self.skill, &params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpClient;
    use crate::skills::runner::SkillRunner;
    use crate::skills::venv::VenvManager;
    use crate::skills::{Language, SkillManifest};
    use haven_common::config::SkillsExecConfig;
    use std::path::PathBuf;

    #[tokio::test]
    async fn mcp_adapter_qualified_name() {
        let client = McpClient::new("test-server", "echo", &[], &[]);
        let info = McpToolInfo {
            name: "greet".into(),
            description: "Greets the user".into(),
            input_schema: serde_json::json!({}),
        };
        let adapter = McpToolAdapter::new(Arc::new(client), "test-server", info);
        assert_eq!(adapter.name(), "mcp::test-server::greet");
        assert_eq!(adapter.description(), "Greets the user");
        assert_eq!(adapter.risk_level(&Value::Null), RiskLevel::Medium);
    }

    #[tokio::test]
    async fn skill_adapter_qualified_name() {
        let manifest = SkillManifest {
            name: "echo".into(),
            description: "Echoes input".into(),
            version: None,
            language: Language::Python,
            allowed_tools: vec![],
            instructions: "".into(),
        };
        let skill = Arc::new(Skill::from_manifest_unchecked(
            manifest,
            PathBuf::from("examples/skills/echo"),
            true,
        ));
        let config = SkillsExecConfig::default();
        let runner = SkillRunner::new(VenvManager::new(config.venv_root.clone()), config);
        let adapter = SkillToolAdapter::new(skill, runner);
        assert_eq!(adapter.name(), "skill::echo");
        assert_eq!(adapter.description(), "Echoes input");
        assert_eq!(adapter.risk_level(&Value::Null), RiskLevel::Medium);
    }

    #[tokio::test]
    async fn mcp_adapter_input_schema() {
        let client = McpClient::new("test-server", "echo", &[], &[]);
        let info = McpToolInfo {
            name: "greet".into(),
            description: "Greets the user".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let adapter = McpToolAdapter::new(Arc::new(client), "test-server", info);
        let schema = adapter.input_schema();
        assert_eq!(schema["type"], "object");
    }

    #[tokio::test]
    async fn skill_adapter_input_schema() {
        let manifest = SkillManifest {
            name: "test".into(),
            description: "desc".into(),
            version: None,
            language: Language::Python,
            allowed_tools: vec![],
            instructions: "".into(),
        };
        let skill = Arc::new(Skill::from_manifest_unchecked(manifest, PathBuf::from("."), true));
        let config = SkillsExecConfig::default();
        let runner = SkillRunner::new(VenvManager::new(config.venv_root.clone()), config);
        let adapter = SkillToolAdapter::new(skill, runner);
        let schema = adapter.input_schema();
        assert!(schema.get("properties").and_then(|p| p.get("params")).is_some());
    }
}
