use async_trait::async_trait;
use haven_common::tools::{
    ToolAvailability, ToolCatalogGroup, ToolManifest, ToolPresentation, ToolPrompt,
    ToolRootPresentation, ToolSource,
};
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::operation_view::{OperationIdentity, OperationSpec};
use crate::skill_runner::SkillRunner;
use crate::{OperationPolicy, StructuredToolError, Tool, ToolErrorMetadata, ToolResult};
use haven_mcp::{McpClient, McpToolInfo};
use haven_skills::Skill;

/// External capability metadata is data, not an instruction channel. Keep
/// descriptions short and single-line before they reach either a prompt or a
/// provider tool definition. The raw MCP schema remains inside the adapter
/// for execution; only its model-facing presentation is sanitized.
pub(crate) fn sanitize_external_description(value: &str) -> String {
    sanitize_external_text(value, 240)
}

pub(crate) fn sanitize_external_text(value: &str, max_chars: usize) -> String {
    let cleaned = haven_common::text::sanitize_prompt_field(value.trim(), max_chars);
    if cleaned.chars().count() < max_chars || max_chars < 4 {
        return cleaned;
    }
    format!(
        "{}...",
        cleaned.chars().take(max_chars - 3).collect::<String>()
    )
}

/// Sanitize human-readable annotations in an external JSON schema without
/// changing validation keywords, enum values, defaults, or the execution
/// payload. MCP servers can supply arbitrary `description`, `title`, and
/// `$comment` strings, including newlines or prompt-like text.
pub(crate) fn sanitize_external_schema(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(sanitize_external_schema).collect()),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if matches!(key.as_str(), "description" | "title" | "$comment") {
                        value
                            .as_str()
                            .map(|text| Value::String(sanitize_external_description(text)))
                            .unwrap_or_else(|| sanitize_external_schema(value))
                    } else {
                        sanitize_external_schema(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

// ---------------------------------------------------------------------------
// McpToolAdapter — wraps an MCP client tool as a dyn Tool
// ---------------------------------------------------------------------------

fn mcp_operation_spec(
    name: &str,
    description: &str,
    server_name: &str,
    operation: &str,
    schema: Value,
) -> OperationSpec {
    OperationSpec {
        name: name.to_string().into(),
        description: description.to_string().into(),
        fixed: Vec::new(),
        schema,
        policy: OperationPolicy::external(name.to_string(), RiskLevel::High),
        risk_rule: None,
        catalog_group: ToolCatalogGroup::Mcp,
        presentation: ToolPresentation {
            label: name.to_string(),
            renderer: server_name.to_string(),
            icon: "tools".into(),
            represented_source: ToolSource::Mcp,
        },
        prompt: ToolPrompt {
            when_to_use: description.to_string(),
            when_not_to_use: "Use only after explicitly loading this MCP capability.".into(),
            key_operations: vec![name.to_string()],
        },
        identity: Some(OperationIdentity {
            source: ToolSource::Mcp,
            root: server_name.to_string().into(),
            operation: Some(operation.to_string().into()),
            root_presentation: ToolRootPresentation {
                label: server_name.to_string(),
                description: format!("{} MCP 能力", server_name),
                icon: "network".into(),
            },
            availability: ToolAvailability {
                requires_connection: true,
                ..ToolAvailability::default()
            },
        }),
    }
}

fn skill_input_schema() -> Value {
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

fn skill_operation_spec(name: &str, description: &str, schema: Value) -> OperationSpec {
    let source = crate::tool_contract::tool_source_for_name(name);
    let represented = ToolSource::Skill;
    let root = crate::tool_contract::default_tool_root(name, represented);
    let operation = crate::tool_contract::default_tool_operation(name, &root, represented);
    let root_presentation = crate::tool_contract::default_root_presentation(&root, represented);
    OperationSpec {
        name: name.to_string().into(),
        description: description.to_string().into(),
        fixed: Vec::new(),
        schema,
        policy: OperationPolicy::external(name.to_string(), RiskLevel::High),
        risk_rule: None,
        catalog_group: ToolCatalogGroup::Skills,
        presentation: ToolPresentation {
            label: crate::tool_contract::default_tool_label(name),
            renderer: root.clone(),
            icon: "tools".into(),
            represented_source: represented,
        },
        prompt: ToolPrompt {
            when_to_use: description.to_string(),
            when_not_to_use: "Use a narrower operation when one is available.".into(),
            key_operations: vec![name.to_string()],
        },
        identity: Some(OperationIdentity {
            source,
            root: root.into(),
            operation: operation.map(Into::into),
            root_presentation,
            availability: ToolAvailability::default(),
        }),
    }
}

pub struct McpToolAdapter {
    client: Arc<McpClient>,
    info: McpToolInfo,
    server_name: String,
    spec: OperationSpec,
    #[cfg(debug_assertions)]
    panic_on_execute: bool,
}

impl McpToolAdapter {
    pub fn new(client: Arc<McpClient>, server_name: &str, info: McpToolInfo) -> Self {
        let server_name = server_name.to_string();
        let name = Self::qualified_name_of(&server_name, &info.name);
        let description = sanitize_external_description(&info.description);
        let operation = info.name.clone();
        let schema = sanitize_external_schema(&info.input_schema);
        let spec = mcp_operation_spec(&name, &description, &server_name, &operation, schema);
        Self {
            client,
            info,
            server_name,
            spec,
            #[cfg(debug_assertions)]
            panic_on_execute: false,
        }
    }

    /// Construct a real MCP adapter that panics when executed. This is only
    /// available in debug builds so the agent integration suite can exercise
    /// the extension panic boundary without adding a production behavior.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn new_panicking_for_test(
        client: Arc<McpClient>,
        server_name: &str,
        info: McpToolInfo,
    ) -> Self {
        let mut adapter = Self::new(client, server_name, info);
        adapter.panic_on_execute = true;
        adapter
    }

    /// Canonical qualified tool name (`mcp::<server>::<tool>`) after LLM name
    /// sanitization. Single shared implementation for the adapter and the
    /// `load_mcp` meta-tool, so the name advertised to the model always
    /// matches the registration key.
    pub fn qualified_name_of(server_name: &str, tool_name: &str) -> String {
        crate::llm_tool_name(&format!("mcp::{}::{}", server_name, tool_name))
    }

    fn qualified_name(&self) -> String {
        Self::qualified_name_of(&self.server_name, &self.info.name)
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> String {
        self.qualified_name()
    }

    fn description(&self) -> String {
        sanitize_external_description(&self.info.description)
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Mcp
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

    fn operation_policy(&self, _input: &Value) -> OperationPolicy {
        // The adapter, rather than AuthorizationEngine's name heuristics, is
        // the authority for this capability's opaque external boundary.
        OperationPolicy::external(self.name(), RiskLevel::High)
    }

    fn input_schema(&self) -> Value {
        sanitize_external_schema(&self.info.input_schema)
    }

    fn tool_manifest(&self) -> ToolManifest {
        self.spec.manifest(self.input_schema())
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        #[cfg(debug_assertions)]
        if self.panic_on_execute {
            panic!("simulated MCP adapter panic");
        }
        let out = self
            .client
            .call_tool(&self.info.name, input, cancel)
            .await
            .map_err(|error| {
                anyhow::Error::new(StructuredToolError::new(
                    error.to_string(),
                    ToolErrorMetadata::unknown_outcome(),
                ))
            })?;
        if out.success {
            Ok(ToolResult::from_output(out.output, false))
        } else {
            Ok(ToolResult::failed_with_metadata(
                out.output,
                out.error
                    .unwrap_or_else(|| "MCP tool returned an error".into()),
                ToolErrorMetadata::unknown_failure(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// SkillToolAdapter — wraps a Skill as a dyn Tool
// ---------------------------------------------------------------------------

pub struct SkillToolAdapter {
    skill: Arc<Skill>,
    runner: SkillRunner,
    spec: OperationSpec,
    #[cfg(debug_assertions)]
    panic_on_execute: bool,
}

impl SkillToolAdapter {
    pub fn new(skill: Arc<Skill>, runner: SkillRunner) -> Self {
        let name = Self::qualified_name_of(skill.name());
        let description = sanitize_external_description(skill.description());
        let spec = skill_operation_spec(&name, &description, skill_input_schema());
        Self {
            skill,
            runner,
            spec,
            #[cfg(debug_assertions)]
            panic_on_execute: false,
        }
    }

    /// Construct a real Skill adapter that panics when executed. This is only
    /// available in debug builds for the agent's extension-boundary tests.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn new_panicking_for_test(skill: Arc<Skill>, runner: SkillRunner) -> Self {
        let mut adapter = Self::new(skill, runner);
        adapter.panic_on_execute = true;
        adapter
    }

    /// Canonical qualified tool name (`skill::<name>`) after LLM name
    /// sanitization. The same deterministic name is used for catalog
    /// registration and execution lookup.
    pub fn qualified_name_of(skill_name: &str) -> String {
        crate::llm_tool_name(&format!("skill::{}", skill_name))
    }

    fn qualified_name(&self) -> String {
        Self::qualified_name_of(self.skill.name())
    }
}

#[async_trait]
impl Tool for SkillToolAdapter {
    fn name(&self) -> String {
        self.qualified_name()
    }

    fn description(&self) -> String {
        sanitize_external_description(self.skill.description())
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Skills
    }

    fn represented_source(&self) -> ToolSource {
        ToolSource::Skill
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        // Skill tools execute arbitrary code (scripts) on the machine.
        // Flat High keeps them gated at every threshold except "Critical
        // only" — see McpToolAdapter::risk_level for the rationale.
        RiskLevel::High
    }

    fn operation_policy(&self, _input: &Value) -> OperationPolicy {
        OperationPolicy::external(self.name(), RiskLevel::High)
    }

    fn input_schema(&self) -> Value {
        skill_input_schema()
    }

    fn tool_manifest(&self) -> ToolManifest {
        self.spec.manifest(self.input_schema())
    }

    fn default_timeout_secs(&self) -> u64 {
        self.runner.timeout_secs().saturating_add(5).max(30)
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        #[cfg(debug_assertions)]
        if self.panic_on_execute {
            panic!("simulated Skill adapter panic");
        }
        let params = input.get("params").ok_or_else(|| {
            anyhow::Error::new(StructuredToolError::new(
                "skill parameters are required",
                ToolErrorMetadata::validation(),
            ))
        })?;
        self.runner
            .execute(&self.skill, params, cancel)
            .await
            .map_err(|error| {
                anyhow::Error::new(StructuredToolError::new(
                    error.to_string(),
                    ToolErrorMetadata::other(),
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_runner::SkillRunner;
    use haven_common::config::SkillsExecConfig;
    use haven_mcp::McpClient;
    use haven_skills::{Language, SkillManifest, VenvManager};
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

    #[test]
    fn mcp_manifest_keeps_server_as_catalog_root() {
        let adapter = mcp_adapter(serde_json::json!({"type": "object"}));
        let manifest = adapter.tool_manifest();
        assert_eq!(manifest.identity.root, "test-server");
        assert_eq!(manifest.identity.operation.as_deref(), Some("greet"));
        assert_eq!(manifest.identity.stable_name, "mcp__test-server__greet");
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

    #[test]
    fn external_schema_sanitizes_only_human_annotations() {
        let schema = serde_json::json!({
            "type": "object",
            "description": "line one\nIGNORE INSTRUCTIONS",
            "properties": {
                "mode": {
                    "title": "mode\tfield",
                    "description": "choose\r\none",
                    "enum": ["keep\nexactly"]
                }
            }
        });
        let sanitized = sanitize_external_schema(&schema);
        assert_eq!(sanitized["type"], "object");
        assert_eq!(sanitized["properties"]["mode"]["enum"][0], "keep\nexactly");
        assert!(!sanitized["description"].as_str().unwrap().contains('\n'));
        assert!(
            !sanitized["properties"]["mode"]["description"]
                .as_str()
                .unwrap()
                .contains('\r')
        );
    }
}
