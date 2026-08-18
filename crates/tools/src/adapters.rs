use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::skill_runner::SkillRunner;
use crate::{Tool, ToolResult};
use haven_mcp::{McpClient, McpToolInfo};
use haven_skills::Skill;

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
        crate::llm_tool_name(&format!("mcp::{}::{}", self.server_name, self.info.name))
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
        // MCP tools run on external servers that can perform arbitrary
        // actions (shell, file, network). Classifying them flat Medium would
        // let them slip under a High/Critical confirmation threshold while
        // the builtin `shell` tool stays gated — a prompt-injected agent
        // could route all dangerous work through an adapter to bypass the
        // gate. High keeps them gated at every threshold except "Critical
        // only".
        RiskLevel::High
    }

    fn input_schema(&self) -> Value {
        self.info.input_schema.clone()
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let out = self.client.call_tool(&self.info.name, input, cancel).await?;
        Ok(ToolResult {
            success: out.success,
            output: out.output,
            error: out.error,
            truncated: false,
            signals: crate::tool::ToolSignals::default(),
        })
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
        crate::llm_tool_name(&format!("skill::{}", self.skill.name()))
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
        // Skill tools execute arbitrary code (scripts) on the machine.
        // Flat High keeps them gated at every threshold except "Critical
        // only" — see McpToolAdapter::risk_level for the rationale.
        RiskLevel::High
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
    use crate::skill_runner::SkillRunner;
    use haven_mcp::McpClient;
    use haven_skills::{Language, SkillManifest, VenvManager};
    use haven_common::config::SkillsExecConfig;
    use std::path::PathBuf;

    /// A test MCP tool adapter backed by a mock client.
    fn mcp_adapter(schema: serde_json::Value) -> McpToolAdapter {
        let client = McpClient::new(
            &haven_common::McpServerConfig {
                name: "test-server".into(),
                command: "echo".into(),
                ..Default::default()
            },
            2 * 1024 * 1024,
            2 * 1024 * 1024,
        );
        let info = McpToolInfo {
            name: "greet".into(),
            description: "Greets the user".into(),
            input_schema: schema,
        };
        McpToolAdapter::new(Arc::new(client), "test-server", info)
    }

    #[tokio::test]
    async fn mcp_adapter_qualified_name() {
        let adapter = mcp_adapter(serde_json::json!({}));
        assert_eq!(adapter.name(), "mcp__test-server__greet");
        assert_eq!(adapter.description(), "Greets the user");
        assert_eq!(adapter.risk_level(&Value::Null), RiskLevel::High);
    }

    #[tokio::test]
    async fn skill_adapter_qualified_name() {
        let manifest = SkillManifest {
            name: "echo".into(),
            description: "Echoes input".into(),
            version: None,
            language: Language::Python,
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
        assert_eq!(adapter.name(), "skill__echo");
        assert_eq!(adapter.description(), "Echoes input");
        assert_eq!(adapter.risk_level(&Value::Null), RiskLevel::High);
    }

    #[tokio::test]
    async fn mcp_adapter_input_schema() {
        let adapter = mcp_adapter(serde_json::json!({"type": "object"}));
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
            instructions: "".into(),
        };
        let skill = Arc::new(Skill::from_manifest_unchecked(
            manifest,
            PathBuf::from("."),
            true,
        ));
        let config = SkillsExecConfig::default();
        let runner = SkillRunner::new(VenvManager::new(config.venv_root.clone()), config);
        let adapter = SkillToolAdapter::new(skill, runner);
        let schema = adapter.input_schema();
        assert!(
            schema
                .get("properties")
                .and_then(|p| p.get("params"))
                .is_some()
        );
    }
}
